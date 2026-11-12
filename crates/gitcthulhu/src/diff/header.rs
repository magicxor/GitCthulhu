//! The diff header (owned by G7a; 09 §1.2): path, diff-type toggles, Stage/Unstage file, close; plus the
//! path-only header and progress bar shown while a load takes longer than 150 ms.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, Hsla, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, div,
};

use super::model::{DiffMode, Uncommitted};
use super::{DiffView, ids};
use crate::backend::{ByteSlice as _, ChangeKind, DiffContent, FileDiff, RelaPath};
use crate::icons::{AppIcon, icon};
use crate::metrics::{diff as m, dp};
use crate::settings::{self, DiffViewMode};
use crate::theme::{ActiveTokens as _, TextStyleExt as _};
use crate::widgets::{IconButton, LinearProgress, SecondaryButton, TooltipExt as _, with_layer};

/// The directory part (without the trailing '/') and the file-name part shown in the header (09 §1.2). Renames
/// show `old → new` (U+2192): the old and new file names when the directory did not change, else the two full
/// paths (Gitnuro repeats the directory in front of two full paths; 21 §6 deviation list of G7a). Deletions show
/// the old path.
pub fn header_path(diff: &FileDiff) -> (String, String) {
    let split = |path: &RelaPath| -> (String, String) {
        let text = path.to_str_lossy().into_owned();
        match text.rfind('/') {
            Some(i) => (text[..i].to_owned(), text[i + 1..].to_owned()),
            None => (String::new(), text),
        }
    };
    let (dir, name) = split(&diff.path);
    match &diff.previous_path {
        Some(previous) if *previous != diff.path => {
            let (old_dir, old_name) = split(previous);
            if old_dir == dir {
                (dir, format!("{old_name} \u{2192} {name}"))
            } else {
                (
                    String::new(),
                    format!(
                        "{} \u{2192} {}",
                        previous.to_str_lossy(),
                        diff.path.to_str_lossy()
                    ),
                )
            }
        }
        _ => (dir, name),
    }
}

/// Whether the header offers the unified/split and hunks/full-file toggles: text diffs of files that are not
/// added or deleted (09 §1.2).
pub fn shows_toggles(diff: &FileDiff) -> bool {
    matches!(diff.content, DiffContent::Text(_))
        && !matches!(
            diff.kind,
            ChangeKind::Added
                | ChangeKind::Deleted
                | ChangeKind::Untracked
                | ChangeKind::IntentToAdd
        )
}

fn toggle(
    id: &'static str,
    role: AppIcon,
    tooltip: &'static str,
    toggled: bool,
    on_click: impl Fn(&mut App) + 'static,
    cx: &App,
) -> AnyElement {
    let tokens = *cx.tokens();
    let hover: Hsla = with_layer(tokens.tertiary_surface, tokens.hover_layer());
    div()
        .id(id)
        .flex_none()
        .size(dp(m::TOGGLE))
        .p(dp(m::TOGGLE_PAD))
        .rounded(dp(crate::metrics::RADIUS))
        .when(toggled, |b| b.bg(tokens.on_background.alpha(0.2)))
        .when(!toggled, |b| b.hover(move |s| s.bg(hover)))
        .cursor_pointer()
        .on_click(move |_, _, cx| {
            // A toggled button is a no-op (09 §1.2).
            if !toggled {
                on_click(cx);
            }
        })
        .delayed_tooltip(tooltip)
        .child(icon(role, m::TOGGLE_ICON, tokens.on_background))
        .test_support()
        .into_any_element()
}

/// The 34 dp header of a loaded diff.
pub(super) fn render(view: &DiffView, cx: &mut Context<DiffView>) -> AnyElement {
    let tokens = *cx.tokens();
    let Some(loaded) = &view.loaded else {
        return div().into_any_element();
    };
    let diff = loaded.doc.diff.clone();
    let (dir, name) = header_path(&diff);
    let s = settings::get_or_default(cx);
    let full_file = s.diff_full_file;
    let split = loaded.doc.mode == DiffMode::Split;
    let toggles = shows_toggles(&diff).then(|| {
        div()
            .flex()
            .flex_none()
            .child(
                div()
                    .flex()
                    .pr(dp(m::TOGGLE_PAIR_GAP))
                    .child(toggle(
                        ids::TOGGLE_HUNKS,
                        AppIcon::Hunks,
                        "Divide by hunks",
                        !full_file,
                        |cx| settings::update(cx, |s| s.diff_full_file = false),
                        cx,
                    ))
                    .child(toggle(
                        ids::TOGGLE_FULL_FILE,
                        AppIcon::FullFile,
                        "View the complete file",
                        full_file,
                        |cx| settings::update(cx, |s| s.diff_full_file = true),
                        cx,
                    )),
            )
            .child(
                div()
                    .flex()
                    .child(toggle(
                        ids::TOGGLE_UNIFIED,
                        AppIcon::Unified,
                        "Unified diff",
                        !split,
                        |cx| settings::update(cx, |s| s.diff_view = DiffViewMode::Unified),
                        cx,
                    ))
                    .child(toggle(
                        ids::TOGGLE_SPLIT,
                        AppIcon::Split,
                        "Split diff",
                        split,
                        |cx| settings::update(cx, |s| s.diff_view = DiffViewMode::Split),
                        cx,
                    )),
            )
    });
    // While another file loads the shown document is stale: no file action on it.
    let stage_target = (!view.stale)
        .then(|| super::model::uncommitted(&loaded.wanted.target))
        .flatten();
    let stage_button = stage_target.map(|list| {
        let ctx = view.ctx.clone();
        let (label, bg, stage) = match list {
            Uncommitted::Staged => ("Unstage file", tokens.error, false),
            Uncommitted::Unstaged => ("Stage file", tokens.primary, true),
        };
        // Unstaging a staged rename restores the old index entry too.
        let mut paths = vec![diff.path.clone()];
        if !stage
            && diff.kind == ChangeKind::Renamed
            && let Some(previous) = diff.previous_path.clone()
        {
            paths.push(previous);
        }
        SecondaryButton::new(ids::STAGE_FILE, label)
            .background(bg)
            .on_click(move |window, cx| super::run_file(&ctx, stage, paths.clone(), window, cx))
    });
    let weak = cx.entity().downgrade();
    let close = IconButton::new(ids::CLOSE, AppIcon::Close)
        .icon_size(m::CLOSE_ICON)
        .on_click(move |window, cx| super::close_and_refocus(&weak, window, cx));
    div()
        .id(ids::HEADER)
        .flex_none()
        .w_full()
        .h(dp(m::HEADER_H))
        .flex()
        .items_center()
        .bg(tokens.tertiary_surface)
        .pl(dp(m::HEADER_PAD_START))
        .pr(dp(m::HEADER_PAD_END))
        .child(
            div()
                .id(ids::HEADER_PATH)
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .overflow_hidden()
                .text_body2()
                .when(!dir.is_empty(), |p| {
                    p.child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .medium()
                            .text_color(tokens.on_background_secondary)
                            .child(SharedString::from(dir)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_color(tokens.on_background_secondary)
                            .child("/"),
                    )
                })
                .child(
                    div()
                        .flex_none()
                        .whitespace_nowrap()
                        .medium()
                        .pr(dp(m::FILE_NAME_PAD_END))
                        .text_color(tokens.on_background)
                        .child(SharedString::from(name)),
                )
                .test_support(),
        )
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(dp(m::HEADER_GROUP_GAP))
                .children(toggles)
                .children(stage_button)
                .child(close),
        )
        .test_support()
        .into_any_element()
}

/// The 40 dp path-only header (09 §1.2 `PathOnlyDiffHeader`) shown while loading or after a load error.
pub(super) fn loading_header(path: String, cx: &mut Context<DiffView>) -> AnyElement {
    let tokens = *cx.tokens();
    let weak = cx.entity().downgrade();
    div()
        .id(ids::LOADING_HEADER)
        .flex_none()
        .w_full()
        .h(dp(m::LOADING_HEADER_H))
        .flex()
        .items_center()
        .bg(tokens.tertiary_surface)
        .px(dp(m::LOADING_HEADER_PAD_H))
        .child(
            div()
                .min_w_0()
                .px(dp(m::LOADING_PATH_PAD_H))
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .text_body2()
                .text_color(tokens.on_background)
                .child(SharedString::from(path)),
        )
        .child(div().flex_1())
        .child(
            IconButton::new(ids::CLOSE, AppIcon::Close)
                .icon_size(m::CLOSE_ICON)
                .on_click(move |window, cx| super::close_and_refocus(&weak, window, cx)),
        )
        .test_support()
        .into_any_element()
}

/// The 4 dp indeterminate progress bar under the path-only header.
pub(super) fn progress_bar() -> AnyElement {
    div()
        .id(ids::PROGRESS)
        .flex_none()
        .w_full()
        .h(dp(m::PROGRESS_H))
        .child(LinearProgress::new())
        .test_support()
        .into_any_element()
}
