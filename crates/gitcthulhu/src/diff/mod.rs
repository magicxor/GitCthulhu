//! The diff view (owned by G7a; 09 §1, spec 21 §4.1): one entity per tab that loads the selected file's diff and
//! renders it with `list(ListState)` (uniform height hint, `scroll_to` jumps, wrapping lines). The interface to
//! G7b (selection, highlighting) is fixed by `selection.rs` and `highlight.rs`: every text cell goes through
//! [`DiffSelection::text_cell`], and [`model::build_doc`] runs the first highlighting pass in the background; the second pass follows in place.
//!
//! Data flow: `RepoUi::diff_target` (or a fixed target for file history) -> background load (`backend::diff::load`,
//! then `build_doc`) through a [`ReadSlot`] -> [`DiffDoc`] -> rows. Status reloads re-read uncommitted diffs and keep
//! the document when the sides did not change; mode switches rebuild the rows only and keep the top line.

use std::sync::Arc;
use std::time::Duration;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, FocusHandle, Focusable, InteractiveElement as _, IntoElement,
    KeyBinding, ListAlignment, ListOffset, ListState, MouseButton, ParentElement as _, Pixels,
    Render, SharedString, Styled as _, Subscription, Task, Window, div, list, px,
};

use crate::backend::{
    ByteSlice as _, DiffTarget, GitError, GitErrorKind, LineSelection, RefreshSet, RelaPath,
    SelectionRequest,
};
use crate::metrics::{self, diff as m};
use crate::settings::{DiffViewMode, SettingsGlobal};
use crate::tab::ops::files::SelectionUnit;
use crate::tab::ui::{CloseableView, RepoUiEvent};
use crate::tab::{ReadSlot, RepoModelEvent, RepoTabEvent, TabCtx};
use crate::theme::ActiveTokens as _;
use crate::widgets::{BoundsCell, ObservedExt as _};

pub mod header;
pub mod highlight;
pub mod model;
pub mod other;
pub mod rows;
pub mod selection;

use model::{Anchor, Controls, DiffDoc, DiffMode};
use selection::DiffSelection;

/// Element ids of the diff view. Row-level ids are tuples `(id, row index)`.
pub mod ids {
    /// The diff view.
    pub const DIFF_VIEW: &str = "diff-view";
    /// The diff header (09 §1.2).
    pub const HEADER: &str = "diff-header";
    /// The path area of the header.
    pub const HEADER_PATH: &str = "diff-header-path";
    /// Toggle "Divide by hunks".
    pub const TOGGLE_HUNKS: &str = "diff-toggle-hunks";
    /// Toggle "View the complete file".
    pub const TOGGLE_FULL_FILE: &str = "diff-toggle-full-file";
    /// Toggle "Unified diff".
    pub const TOGGLE_UNIFIED: &str = "diff-toggle-unified";
    /// Toggle "Split diff".
    pub const TOGGLE_SPLIT: &str = "diff-toggle-split";
    /// "Stage file" / "Unstage file".
    pub const STAGE_FILE: &str = "diff-stage-file";
    /// Close button of the header.
    pub const CLOSE: &str = "diff-close";
    /// The path-only header shown while loading or after an error.
    pub const LOADING_HEADER: &str = "diff-loading-header";
    /// The loading progress bar.
    pub const PROGRESS: &str = "diff-progress";
    /// The virtual list of rows.
    pub const LIST: &str = "diff-list";
    /// The list's scrollbar.
    pub const SCROLLBAR: &str = "diff-scrollbar";
    /// A hunk header row `(HUNK, row)`.
    pub const HUNK: &str = "diff-hunk";
    /// "Stage hunk" `(HUNK_STAGE, row)`.
    pub const HUNK_STAGE: &str = "diff-hunk-stage";
    /// "Unstage hunk" `(HUNK_UNSTAGE, row)`.
    pub const HUNK_UNSTAGE: &str = "diff-hunk-unstage";
    /// "Discard hunk" `(HUNK_DISCARD, row)`.
    pub const HUNK_DISCARD: &str = "diff-hunk-discard";
    /// A line row (unified) or a pair row (split) `(LINE, row)`.
    pub const LINE: &str = "diff-line";
    /// Old line-number cell `(NUM_OLD, row)`.
    pub const NUM_OLD: &str = "diff-num-old";
    /// New line-number cell `(NUM_NEW, row)`.
    pub const NUM_NEW: &str = "diff-num-new";
    /// Text cell, unified `(TEXT, row)`.
    pub const TEXT: &str = "diff-text";
    /// Text cell, split old side `(TEXT_OLD, row)`.
    pub const TEXT_OLD: &str = "diff-text-old";
    /// Text cell, split new side `(TEXT_NEW, row)`.
    pub const TEXT_NEW: &str = "diff-text-new";
    /// Grey filler of a missing split side `(FILLER_OLD | FILLER_NEW, row)`.
    pub const FILLER_OLD: &str = "diff-filler-old";
    /// See [`FILLER_OLD`].
    pub const FILLER_NEW: &str = "diff-filler-new";
    /// The old half of a split row `(SIDE_OLD, row)`.
    pub const SIDE_OLD: &str = "diff-side-old";
    /// The new half of a split row `(SIDE_NEW, row)`.
    pub const SIDE_NEW: &str = "diff-side-new";
    /// The 4 dp divider of a split row `(SPLIT_DIVIDER, row)`.
    pub const SPLIT_DIVIDER: &str = "diff-split-divider";
    /// The hover chip, unified `(CHIP, row)`.
    pub const CHIP: &str = "diff-chip";
    /// The hover chip of the old side, split `(CHIP_OLD, row)`.
    pub const CHIP_OLD: &str = "diff-chip-old";
    /// The hover chip of the new side, split `(CHIP_NEW, row)`.
    pub const CHIP_NEW: &str = "diff-chip-new";
    /// "No content changes to display" (also too large, type change, mode only, empty).
    pub const NO_CHANGES: &str = "diff-no-changes";
    /// Load error view.
    pub const ERROR: &str = "diff-error";
    /// Binary file view.
    pub const BINARY: &str = "diff-binary";
    /// Old / new image.
    pub const IMAGE_OLD: &str = "diff-image-old";
    /// See [`IMAGE_OLD`].
    pub const IMAGE_NEW: &str = "diff-image-new";
    /// Submodule view.
    pub const SUBMODULE: &str = "diff-submodule";
    /// "Open submodule".
    pub const OPEN_SUBMODULE: &str = "diff-open-submodule";
}

/// Key context of the diff view (navigation keys here, copy / select all from the selection).
pub use selection::DIFF_CONTEXT;

gpui_kit::actions!(
    gitcthulhu_diff,
    [
        /// Scroll up by one line.
        DiffLineUp,
        /// Scroll down by one line.
        DiffLineDown,
        /// Scroll up by one page.
        DiffPageUp,
        /// Scroll down by one page.
        DiffPageDown,
        /// Scroll to the top.
        DiffTop,
        /// Scroll to the bottom.
        DiffBottom,
    ]
);

/// Where the shown target comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    /// The tab's single selected file (`RepoUi::diff_target`).
    Selection,
    /// A target set by the owner (file history); no header.
    Fixed,
}

/// The requested target.
#[derive(Clone, Debug, PartialEq)]
struct Wanted {
    target: DiffTarget,
    /// The commit is a stash (its untracked files live in the third parent).
    stash: bool,
}

/// A shown document.
pub(crate) struct Loaded {
    wanted: Wanted,
    full_file: bool,
    doc: Arc<DiffDoc>,
    images: Option<Arc<other::Images>>,
}

/// Result of a background load.
enum LoadResult {
    /// Same sides as the shown document: keep it (and the scroll position).
    Unchanged,
    Built {
        doc: DiffDoc,
        images: Option<Arc<other::Images>>,
    },
    Failed(GitError),
}

/// Per-frame values the row renderer needs (computed once in `render`).
#[derive(Clone, Copy)]
struct Frame {
    controls: Controls,
    /// Width of one mono digit at 13 dp.
    advance: Pixels,
    /// Fixed part of a number column (padding start + end).
    number_pad: Pixels,
    /// Height of one unwrapped line.
    line_h: Pixels,
}

/// The settings the view reacts to.
#[derive(Clone, Copy, PartialEq)]
struct Seen {
    mode: DiffViewMode,
    full_file: bool,
    scale: f32,
}

/// The diff view of a repository tab.
pub struct DiffView {
    ctx: TabCtx,
    source: Source,
    focus: FocusHandle,
    wanted: Option<Wanted>,
    loaded: Option<Loaded>,
    error: Option<SharedString>,
    /// A load for a new target runs; the shown document (if any) belongs to the previous target.
    stale: bool,
    loading: bool,
    show_loading: bool,
    load_slot: ReadSlot,
    build_slot: ReadSlot,
    /// The second highlighting pass of the shown document (it fills the document's `SyntaxCell` in place).
    syntax_slot: ReadSlot,
    loading_timer: Option<Task<()>>,
    generation: u64,
    list: ListState,
    selection: DiffSelection,
    seen: Seen,
    rem_size: Pixels,
    frame: Frame,
    image_cells: [BoundsCell; 2],
    _subscriptions: Vec<Subscription>,
}

fn mode_of(view: DiffViewMode) -> DiffMode {
    match view {
        DiffViewMode::Unified => DiffMode::Unified,
        DiffViewMode::Split => DiffMode::Split,
    }
}

fn seen_settings(cx: &App) -> Seen {
    let s = crate::settings::get_or_default(cx);
    Seen {
        mode: s.diff_view,
        full_file: s.diff_full_file,
        scale: s.scale,
    }
}

impl DiffView {
    /// The diff view of the tab of `ctx`: shows the tab's single selected file.
    pub fn new(ctx: TabCtx, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self::with_source(ctx, Source::Selection, cx);
        // 09 §1.1: the diff becomes the most recent closeable view when it gains focus too (Esc closes it first).
        let focus = this.focus.clone();
        this._subscriptions
            .push(cx.on_focus_in(&focus, window, |this: &mut Self, _, cx| {
                this.on_focus_in(cx)
            }));
        this
    }

    fn on_focus_in(&mut self, cx: &mut Context<Self>) {
        if self.wanted.is_none() {
            return;
        }
        self.ctx.ui.update(cx, |ui, cx| {
            if ui.views.as_slice().last() != Some(&CloseableView::Diff) {
                ui.push_view(CloseableView::Diff, cx);
            }
        });
    }

    /// A read-only diff view without header that shows the target given to [`Self::set_target`] (file history,
    /// 09 §2.2). It follows the global unified/split and full-file settings.
    pub fn read_only(ctx: TabCtx, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::with_source(ctx, Source::Fixed, cx)
    }

    fn with_source(ctx: TabCtx, source: Source, cx: &mut Context<Self>) -> Self {
        let mut subscriptions = Vec::new();
        if source == Source::Selection {
            subscriptions.push(cx.subscribe(
                &ctx.ui,
                |this: &mut Self, _, ev: &RepoUiEvent, cx| match ev {
                    RepoUiEvent::FilesChanged
                    | RepoUiEvent::SelectionChanged
                    | RepoUiEvent::CenterChanged => this.sync_target(cx),
                    RepoUiEvent::ViewStackChanged | RepoUiEvent::TogglesChanged => {}
                },
            ));
        }
        subscriptions.push(cx.subscribe(
            &ctx.events,
            |this: &mut Self, _, ev: &RepoTabEvent, cx| {
                if matches!(ev, RepoTabEvent::StatusReloaded | RepoTabEvent::ReloadDiff) {
                    this.on_status_reloaded(cx);
                }
            },
        ));
        // Repository state gates the hunk buttons; submodule state gates "Open submodule".
        subscriptions.push(
            cx.subscribe(&ctx.model, |_: &mut Self, _, ev: &RepoModelEvent, cx| {
                if ev
                    .0
                    .intersects(RefreshSet::REPO_STATE | RefreshSet::SUBMODULES)
                {
                    cx.notify();
                }
            }),
        );
        subscriptions
            .push(cx.observe_global::<SettingsGlobal>(|this: &mut Self, cx| this.on_settings(cx)));
        let rem_size = px(metrics::BASE_REM);
        let line_h = rem_size * (m::LINE_H / metrics::BASE_REM);
        let selection = DiffSelection::new(cx);
        let focus = cx.focus_handle();
        selection.focus_on_drag(&focus, cx);
        Self {
            focus,
            source,
            wanted: None,
            loaded: None,
            error: None,
            stale: false,
            loading: false,
            show_loading: false,
            load_slot: ReadSlot::default(),
            build_slot: ReadSlot::default(),
            syntax_slot: ReadSlot::default(),
            loading_timer: None,
            generation: 0,
            list: ListState::new(0, ListAlignment::Top, px(m::LIST_OVERDRAW))
                .with_uniform_item_height(line_h),
            selection,
            seen: seen_settings(cx),
            rem_size,
            frame: Frame {
                controls: Controls::default(),
                advance: px(0.),
                number_pad: px(0.),
                line_h,
            },
            image_cells: Default::default(),
            _subscriptions: subscriptions,
            ctx,
        }
    }

    /// The tab of this view.
    pub fn ctx(&self) -> &TabCtx {
        &self.ctx
    }

    /// The shown document, if any.
    pub fn doc(&self) -> Option<&Arc<DiffDoc>> {
        self.loaded.as_ref().map(|l| &l.doc)
    }

    /// The list state (scroll position).
    pub fn list_state(&self) -> &ListState {
        &self.list
    }

    /// The text selection of this view (G7b).
    pub fn selection(&self) -> &DiffSelection {
        &self.selection
    }

    /// The text selection of this view, mutable (G7b: select all, clear).
    pub fn selection_mut(&mut self) -> &mut DiffSelection {
        &mut self.selection
    }

    /// A load runs.
    pub fn is_loading(&self) -> bool {
        self.loading
    }

    /// Read-only views: shows `target` (`None` clears the view). A different target scrolls to the top.
    pub fn set_target(&mut self, target: Option<DiffTarget>, cx: &mut Context<Self>) {
        let wanted = target.map(|target| Wanted {
            target,
            stash: false,
        });
        self.apply_wanted(wanted, cx);
    }

    /// Scrolls so that row `ix` is at the top (all programmatic jumps use `scroll_to`, spec 21 §4.1).
    pub fn scroll_to_row(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.list.scroll_to(ListOffset {
            item_ix: ix,
            offset_in_item: px(0.),
        });
        cx.notify();
    }

    fn wanted_from_selection(&self, cx: &App) -> Option<Wanted> {
        let ui = self.ctx.ui.read(cx);
        let model = self.ctx.model.read(cx);
        let target = ui.diff_target(model)?;
        let stash = matches!(target, DiffTarget::Commit { .. })
            && ui.selection.commit().is_some_and(|(_, stash)| stash);
        Some(Wanted { target, stash })
    }

    fn sync_target(&mut self, cx: &mut Context<Self>) {
        if self.source != Source::Selection {
            return;
        }
        let wanted = self.wanted_from_selection(cx);
        self.apply_wanted(wanted, cx);
    }

    fn apply_wanted(&mut self, wanted: Option<Wanted>, cx: &mut Context<Self>) {
        if wanted == self.wanted {
            return;
        }
        self.wanted = wanted;
        self.error = None;
        if self.wanted.is_none() {
            self.load_slot.cancel();
            self.build_slot.cancel();
            self.syntax_slot.cancel();
            self.loaded = None;
            self.loading = false;
            self.show_loading = false;
            self.stale = false;
            self.loading_timer = None;
            self.selection.clear(cx);
            self.list.reset(0);
            cx.notify();
            return;
        }
        self.stale = self.loaded.is_some();
        self.start_load(true, cx);
    }

    fn on_status_reloaded(&mut self, cx: &mut Context<Self>) {
        if self.source == Source::Selection {
            let wanted = self.wanted_from_selection(cx);
            if wanted != self.wanted {
                self.apply_wanted(wanted, cx);
                return;
            }
        }
        let uncommitted = self
            .wanted
            .as_ref()
            .is_some_and(|w| model::uncommitted(&w.target).is_some());
        if uncommitted && !self.stale {
            self.start_load(false, cx);
        }
    }

    fn on_settings(&mut self, cx: &mut Context<Self>) {
        let seen = seen_settings(cx);
        let before = std::mem::replace(&mut self.seen, seen);
        if seen.scale != before.scale {
            self.list.remeasure();
        }
        if seen.full_file != before.full_file {
            if self.wanted.is_some() {
                self.start_load(false, cx);
            }
        } else if seen.mode != before.mode {
            self.ensure_mode(cx);
        }
        cx.notify();
    }

    /// Starts reading the wanted target. `reset`: a new target (scroll to the top); otherwise the shown document
    /// is kept if the sides did not change, and the top line stays in place if they did.
    fn start_load(&mut self, reset: bool, cx: &mut Context<Self>) {
        let Some(wanted) = self.wanted.clone() else {
            return;
        };
        let Some(repo) = self.ctx.backend.repo().cloned() else {
            return;
        };
        // A load replacing a pending new-target load keeps its scroll reset (the shown document, if any, belongs
        // to another file).
        let reset = reset || self.stale || self.loaded.is_none();
        let settings = crate::settings::get_or_default(cx);
        let full_file = settings.diff_full_file;
        let mode = mode_of(settings.diff_view);
        let previous = if reset {
            None
        } else {
            self.loaded
                .as_ref()
                .filter(|l| l.full_file == full_file && l.wanted == wanted)
                .map(|l| (l.doc.diff.clone(), l.doc.mode))
        };
        self.generation += 1;
        let generation = self.generation;
        self.build_slot.cancel();
        self.loading = true;
        if reset {
            self.show_loading = false;
            self.start_loading_timer(cx);
        }
        let apply_wanted = wanted.clone();
        self.load_slot.start(
            cx,
            |v: &mut Self| &mut v.load_slot,
            move |cancel| match crate::backend::diff::load(
                &repo,
                &wanted.target,
                wanted.stash,
                full_file,
                cancel,
            ) {
                Err(err) => LoadResult::Failed(err),
                Ok(diff) => {
                    if let Some((prev, prev_mode)) = previous
                        && model::same_content(&prev, &diff)
                        && model::effective_mode(diff.kind, mode) == prev_mode
                    {
                        return LoadResult::Unchanged;
                    }
                    let diff = Arc::new(diff);
                    let images = other::decode_images(&diff).map(Arc::new);
                    let doc = model::build_doc(diff, mode, generation, cancel);
                    LoadResult::Built { doc, images }
                }
            },
            move |this: &mut Self, result, cx| {
                this.finish_load(apply_wanted, full_file, reset, result, cx)
            },
        );
    }

    fn start_loading_timer(&mut self, cx: &mut Context<Self>) {
        self.loading_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(m::LOADING_DELAY_MS))
                .await;
            let _ = this.update(cx, |this: &mut Self, cx| {
                if this.loading && (this.loaded.is_none() || this.stale) {
                    this.show_loading = true;
                    cx.notify();
                }
            });
        }));
    }

    fn finish_load(
        &mut self,
        wanted: Wanted,
        full_file: bool,
        reset: bool,
        result: LoadResult,
        cx: &mut Context<Self>,
    ) {
        self.loading = false;
        self.show_loading = false;
        self.loading_timer = None;
        match result {
            LoadResult::Unchanged => {}
            LoadResult::Built { doc, images } => {
                self.error = None;
                self.stale = false;
                let anchor = if reset { None } else { self.current_anchor() };
                self.install(
                    Loaded {
                        wanted,
                        full_file,
                        doc: Arc::new(doc),
                        images,
                    },
                    anchor,
                    cx,
                );
                self.start_full_highlight(cx);
                self.ensure_mode(cx);
            }
            LoadResult::Failed(err) => self.load_failed(&wanted, err, cx),
        }
        cx.notify();
    }

    /// Starts the second highlighting pass for the shown document when the first one stopped at its limit. The
    /// result is stored in the document's shared syntax cell, so the document, its rows and the text selection stay
    /// as they are; only the view repaints. A newer document cancels the pass.
    fn start_full_highlight(&mut self, cx: &mut Context<Self>) {
        let Some(loaded) = &self.loaded else {
            return;
        };
        if loaded.doc.syntax.is_complete() {
            self.syntax_slot.cancel();
            return;
        }
        let doc = loaded.doc.clone();
        self.syntax_slot.start(
            cx,
            |v: &mut Self| &mut v.syntax_slot,
            move |cancel| doc.highlight_full(cancel),
            |_: &mut Self, changed, cx| {
                if changed {
                    cx.notify();
                }
            },
        );
    }

    fn load_failed(&mut self, wanted: &Wanted, err: GitError, cx: &mut Context<Self>) {
        match err.kind() {
            GitErrorKind::Cancelled => {}
            // 09 §1.1: the file has no changes any more (DiffNotFound) -> the pane closes itself.
            GitErrorKind::NotFound
                if self.source == Source::Selection
                    && model::uncommitted(&wanted.target).is_some() =>
            {
                tracing::debug!("diff target vanished: {err}");
                self.loaded = None;
                self.syntax_slot.cancel();
                self.stale = false;
                self.selection.clear(cx);
                self.ctx.ui.update(cx, |ui, cx| ui.clear_files(cx));
            }
            _ => {
                tracing::warn!("diff load failed: {err}");
                self.loaded = None;
                self.syntax_slot.cancel();
                self.stale = false;
                self.selection.clear(cx);
                self.list.reset(0);
                self.error = Some(SharedString::from(format!("Cannot show this diff: {err}")));
            }
        }
    }

    /// The top row's anchor and its offset.
    fn current_anchor(&self) -> Option<(Anchor, Pixels)> {
        let loaded = self.loaded.as_ref()?;
        let top = self.list.logical_scroll_top();
        loaded
            .doc
            .anchor(top.item_ix)
            .map(|a| (a, top.offset_in_item))
    }

    fn install(
        &mut self,
        loaded: Loaded,
        anchor: Option<(Anchor, Pixels)>,
        cx: &mut Context<Self>,
    ) {
        self.selection.reset(&loaded.doc, cx);
        self.list
            .reset_with_uniform_height(loaded.doc.rows.len(), self.frame.line_h);
        if let Some((anchor, offset)) = anchor
            && let Some(ix) = loaded.doc.find_anchor(&anchor)
        {
            self.list.scroll_to(ListOffset {
                item_ix: ix,
                offset_in_item: offset,
            });
        }
        self.loaded = Some(loaded);
    }

    /// Rebuilds the rows when the unified/split setting differs from the shown document (no reload).
    fn ensure_mode(&mut self, cx: &mut Context<Self>) {
        let Some(loaded) = &self.loaded else {
            return;
        };
        let mode = mode_of(crate::settings::get_or_default(cx).diff_view);
        if model::effective_mode(loaded.doc.diff.kind, mode) == loaded.doc.mode
            || loaded.doc.text().is_none()
        {
            return;
        }
        let doc = loaded.doc.clone();
        let base = doc.generation;
        self.generation += 1;
        let generation = self.generation;
        self.build_slot.start(
            cx,
            |v: &mut Self| &mut v.build_slot,
            move |_| model::rebuild_mode(&doc, mode, generation),
            move |this: &mut Self, doc, cx| {
                // A newer document (reload) arrived meanwhile: it was built in the current mode already.
                if this.loaded.as_ref().map(|l| l.doc.generation) != Some(base) {
                    return;
                }
                let anchor = this.current_anchor();
                let Some(current) = this.loaded.take() else {
                    return;
                };
                this.install(
                    Loaded {
                        doc: Arc::new(doc),
                        ..current
                    },
                    anchor,
                    cx,
                );
                cx.notify();
            },
        );
    }

    /// Closes the diff (header X): deselects the file (09 §1.1).
    fn close(&mut self, cx: &mut Context<Self>) {
        if self.source == Source::Selection {
            self.ctx.ui.update(cx, |ui, cx| ui.clear_files(cx));
        }
    }
}

/// The header's close button: closes the diff and gives the keyboard back to the repository frame (the focused diff
/// is gone, and focus would otherwise fall back to the window root).
fn close_and_refocus(view: &gpui_kit::WeakEntity<DiffView>, window: &mut Window, cx: &mut App) {
    let Ok(ctx) = view.update(cx, |view, cx| {
        view.close(cx);
        view.ctx.clone()
    }) else {
        return;
    };
    ctx.with_tab(window, cx, |tab, window, cx| tab.focus(window, cx));
}

impl DiffView {
    fn line_up(&mut self, _: &DiffLineUp, _: &mut Window, cx: &mut Context<Self>) {
        self.list.scroll_by(-self.frame.line_h);
        cx.notify();
    }

    fn line_down(&mut self, _: &DiffLineDown, _: &mut Window, cx: &mut Context<Self>) {
        self.list.scroll_by(self.frame.line_h);
        cx.notify();
    }

    fn page_up(&mut self, _: &DiffPageUp, _: &mut Window, cx: &mut Context<Self>) {
        let page = self.list.viewport_bounds().size.height;
        self.list.scroll_by(-page);
        cx.notify();
    }

    fn page_down(&mut self, _: &DiffPageDown, _: &mut Window, cx: &mut Context<Self>) {
        let page = self.list.viewport_bounds().size.height;
        self.list.scroll_by(page);
        cx.notify();
    }

    fn top(&mut self, _: &DiffTop, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_to_row(0, cx);
    }

    fn bottom(&mut self, _: &DiffBottom, _: &mut Window, cx: &mut Context<Self>) {
        self.list.scroll_to_end();
        cx.notify();
    }

    fn compute_frame(&mut self, window: &mut Window, cx: &App) {
        self.rem_size = window.rem_size();
        let mono = gpui_kit::font(crate::fonts::FONT_MONO);
        let text_system = window.text_system();
        let font_id = text_system.resolve_font(&mono);
        let size = metrics::dpx(13., window);
        let advance = text_system
            .advance(font_id, size, '0')
            .map(|a| a.width)
            .unwrap_or(size * 0.6);
        let repo_safe = self
            .ctx
            .model
            .read(cx)
            .state
            .value
            .as_ref()
            .is_some_and(|s| s.in_progress.is_none());
        let controls = match &self.loaded {
            Some(loaded) if !self.stale && self.source == Source::Selection => {
                model::controls(&loaded.doc.diff, repo_safe)
            }
            _ => Controls::default(),
        };
        self.frame = Frame {
            controls,
            advance,
            number_pad: metrics::dpx(m::NUMBER_PAD_START + m::NUMBER_PAD_END, window),
            line_h: metrics::dpx(m::LINE_H, window),
        };
    }

    fn render_body(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(loaded) = &self.loaded else {
            return div().into_any_element();
        };
        let diff = loaded.doc.diff.clone();
        match &diff.content {
            crate::backend::DiffContent::Text(_) if loaded.doc.rows.is_empty() => {
                other::no_changes("No content changes to display", cx)
            }
            crate::backend::DiffContent::Text(_) => div()
                .flex_1()
                .min_h_0()
                .w_full()
                .relative()
                .child(
                    crate::widgets::observed(ids::LIST).size_full().child(
                        list(
                            self.list.clone(),
                            cx.processor(|this: &mut Self, ix, window, cx| {
                                rows::render_row(this, ix, window, cx)
                            }),
                        )
                        .size_full(),
                    ),
                )
                .child(crate::widgets::scroll::list_scrollbar(
                    ids::SCROLLBAR,
                    &self.list,
                ))
                .into_any_element(),
            _ => other::render_other(self, &diff, cx),
        }
    }
}

impl Focusable for DiffView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for DiffView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.compute_frame(window, cx);
        let tokens = *cx.tokens();
        let show_header = self.source == Source::Selection;
        let root = div()
            .key_context(DIFF_CONTEXT)
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(tokens.background)
            .on_action(cx.listener(Self::line_up))
            .on_action(cx.listener(Self::line_down))
            .on_action(cx.listener(Self::page_up))
            .on_action(cx.listener(Self::page_down))
            .on_action(cx.listener(Self::top))
            .on_action(cx.listener(Self::bottom))
            // Clicks on gutters and headers focus the diff too, so copy and select all reach it.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| window.focus(&this.focus, cx)),
            );
        let root = self.selection.key_actions(root);
        let root = if self.show_loading || (self.loaded.is_none() && self.error.is_some()) {
            let path = self
                .wanted
                .as_ref()
                .map(|w| target_path(&w.target).to_str_lossy().into_owned())
                .unwrap_or_default();
            let error = self.error.clone();
            root.when(show_header, |r| r.child(header::loading_header(path, cx)))
                .when(error.is_none(), |r| r.child(header::progress_bar()))
                .when_some(error, |r, error| r.child(other::error_view(error, cx)))
        } else if self.loaded.is_some() {
            root.when(show_header, |r| r.child(header::render(self, cx)))
                .child(self.render_body(cx))
        } else {
            root
        };
        root.observed(ids::DIFF_VIEW)
    }
}

/// The path of a target.
fn target_path(target: &DiffTarget) -> &RelaPath {
    match target {
        DiffTarget::Unstaged { path, .. }
        | DiffTarget::Staged { path, .. }
        | DiffTarget::Commit { path, .. } => path,
    }
}

/// What a selection action does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SelectionOp {
    Stage,
    Unstage,
    Discard,
}

/// Runs a hunk or line operation through the tab's catalogue (G5: `stage_selection`, `unstage_selection`,
/// `discard_selection`). Call from event handlers only.
pub(crate) fn run_selection(
    ctx: &TabCtx,
    op: SelectionOp,
    req: SelectionRequest,
    unit: SelectionUnit,
    window: &mut Window,
    cx: &mut App,
) {
    #[cfg(test)]
    recorder::record(recorder::Recorded::Selection(op, req.clone(), unit));
    ctx.with_tab(window, cx, move |tab, window, cx| {
        match op {
            SelectionOp::Stage => tab.stage_selection(req, unit, window, cx),
            SelectionOp::Unstage => tab.unstage_selection(req, unit, window, cx),
            // Discard selections only exist for modified (tracked) files (09 §1.3).
            SelectionOp::Discard => tab.discard_selection(req, unit, false, window, cx),
        };
    });
}

/// Stages (`stage = true`) or unstages whole files from the header button.
pub(crate) fn run_file(
    ctx: &TabCtx,
    stage: bool,
    paths: Vec<RelaPath>,
    window: &mut Window,
    cx: &mut App,
) {
    #[cfg(test)]
    recorder::record(recorder::Recorded::File(stage, paths.clone()));
    ctx.with_tab(window, cx, move |tab, window, cx| {
        if stage {
            tab.stage_paths(paths, window, cx);
        } else {
            tab.unstage_paths(paths, window, cx);
        }
    });
}

/// A hunk selection of `doc`'s hunk `hunk`.
pub(crate) fn hunk_request(doc: &DiffDoc, hunk: u32) -> Option<SelectionRequest> {
    let header = doc.text()?.hunks.get(hunk as usize)?.header;
    Some(model::selection_request(
        &doc.diff,
        LineSelection::Hunks(vec![header]),
    ))
}

/// A one-line selection of `doc`'s line `line` (`None` for context lines).
pub(crate) fn line_request(doc: &DiffDoc, line: u32) -> Option<SelectionRequest> {
    let line_ref = model::line_ref(doc.line(line)?)?;
    Some(model::selection_request(
        &doc.diff,
        LineSelection::Lines(vec![line_ref]),
    ))
}

/// Opens `path` with the system's default application (tests record it instead: platform side effects panic on
/// the test platform).
pub(crate) fn open_with_system(path: &std::path::Path, cx: &mut App) {
    #[cfg(test)]
    {
        let _ = cx;
        recorder::record(recorder::Recorded::Opened(path.to_path_buf()));
    }
    #[cfg(not(test))]
    cx.open_with_system(path);
}

/// Test recorder of the actions the view starts (G5's selection operations are stubs until it merges).
#[cfg(test)]
pub(crate) mod recorder {
    use std::cell::RefCell;
    use std::path::PathBuf;

    use super::SelectionOp;
    use crate::backend::{RelaPath, SelectionRequest};
    use crate::tab::ops::files::SelectionUnit;

    /// One recorded action.
    #[derive(Clone, Debug)]
    pub(crate) enum Recorded {
        Selection(SelectionOp, SelectionRequest, SelectionUnit),
        File(bool, Vec<RelaPath>),
        Opened(PathBuf),
    }

    thread_local! {
        static CALLS: RefCell<Vec<Recorded>> = const { RefCell::new(Vec::new()) };
    }

    pub(crate) fn record(call: Recorded) {
        CALLS.with(|c| c.borrow_mut().push(call));
    }

    /// Takes the recorded actions.
    pub(crate) fn take() -> Vec<Recorded> {
        CALLS.with(|c| std::mem::take(&mut *c.borrow_mut()))
    }
}

/// Registers the diff view's navigation keys in the `Diff` context (21 §2.12).
pub fn register(cx: &mut App) {
    let ctx = Some(DIFF_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("up", DiffLineUp, ctx),
        KeyBinding::new("down", DiffLineDown, ctx),
        KeyBinding::new("pageup", DiffPageUp, ctx),
        KeyBinding::new("pagedown", DiffPageDown, ctx),
        KeyBinding::new("home", DiffTop, ctx),
        KeyBinding::new("end", DiffBottom, ctx),
    ]);
}

#[cfg(test)]
mod tests;
