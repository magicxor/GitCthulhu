//! PR / CI badges (D0.1; owned by X4; spec 21 §5.5). Rows and chips only read the controller's badge map; never a
//! forge call during render.
//!
//! * Side panel local-branch rows: `[PR pill][CI dot]` before Gitnuro's "HEAD" text.
//! * Log branch chips: a PR segment at the end of the chip, clipped by the chip's rounding.
//! * Colours are derived from the theme tokens: open = `add_file`, draft = `on_background_secondary`, merged = the
//!   hue of `add_file` turned to purple, closed = `deleted_file`; CI: passed = `add_file`, running/queued =
//!   `modified_file`, failed = `deleted_file`.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Hsla, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, div,
};

use crate::backend::forge::{CiState, PrState, ReviewBadge};
use crate::backend::{ByteSlice as _, FullName};
use crate::log_view::chips::{ChipEnd, round_chip_end};
use crate::metrics::dp;
use crate::tab::TabCtx;
use crate::theme::{ActiveTokens as _, TextStyleExt as _, Tokens};
use crate::widgets::TooltipExt as _;

/// Sizes of the badges (21 §5.5), in dp.
pub mod metrics {
    /// Height of the PR pill in a side panel row.
    pub const PILL_H: f32 = 18.;
    /// Corner radius of the PR pill.
    pub const PILL_RADIUS: f32 = 9.;
    /// Horizontal padding of the PR pill.
    pub const PILL_PAD_H: f32 = 6.;
    /// The CI dot.
    pub const CI_DOT: f32 = 8.;
    /// Margin before the CI dot in a row.
    pub const CI_DOT_MARGIN_START: f32 = 6.;
    /// Margin after the badge when "HEAD" or the ahead/behind text follows.
    pub const ROW_MARGIN_END: f32 = 8.;
    /// Margin after the badge when it is the last thing in the row (the padding "HEAD" would have).
    pub const ROW_MARGIN_END_LAST: f32 = 16.;
    /// Height of the PR segment of a log chip (the chip's own height).
    pub const SEGMENT_H: f32 = crate::metrics::log::CHIP_H;
    /// Horizontal padding of the segment.
    pub const SEGMENT_PAD_H: f32 = 6.;
    /// The border of the CI dot in a segment.
    pub const SEGMENT_DOT_BORDER: f32 = 1.;
    /// The gap between the CI dot and the number in a segment.
    pub const SEGMENT_DOT_GAP: f32 = 4.;
    /// Alpha of the pill's background.
    pub const PILL_BG_ALPHA: f32 = 0.2;
}

/// Element ids (tests, QA).
pub mod ids {
    use gpui_kit::SharedString;

    /// The whole badge (pill and dot) of a side panel row.
    pub fn row_badge(branch: &str) -> SharedString {
        format!("forge-row-badge-{branch}").into()
    }

    /// The PR pill of a side panel row.
    pub fn pill(branch: &str) -> SharedString {
        format!("forge-pill-{branch}").into()
    }

    /// The CI dot of a side panel row.
    pub fn row_ci_dot(branch: &str) -> SharedString {
        format!("forge-row-ci-{branch}").into()
    }

    /// The PR segment of a log chip.
    pub fn segment(branch: &str) -> SharedString {
        format!("forge-segment-{branch}").into()
    }

    /// The CI dot of a log chip's segment.
    pub fn segment_ci_dot(branch: &str) -> SharedString {
        format!("forge-segment-ci-{branch}").into()
    }
}

/// The colour of a pull request state.
pub fn state_color(state: PrState, tokens: &Tokens) -> Hsla {
    match state {
        PrState::Open => tokens.add_file,
        PrState::Draft => tokens.on_background_secondary,
        PrState::Merged => Hsla {
            h: 0.75,
            ..tokens.add_file
        },
        PrState::Closed => tokens.deleted_file,
    }
}

/// The colour of a CI state.
pub fn ci_color(state: CiState, tokens: &Tokens) -> Hsla {
    match state {
        CiState::Passed => tokens.add_file,
        CiState::Running => tokens.modified_file,
        CiState::Failed => tokens.deleted_file,
    }
}

/// The CI state a badge shows: open and draft reviews only (a merged review's checks no longer matter).
pub fn ci_state_of(badge: &ReviewBadge) -> Option<CiState> {
    match badge.state {
        PrState::Open | PrState::Draft => badge.checks.state(),
        PrState::Merged | PrState::Closed => None,
    }
}

/// "#123".
pub fn label_of(badge: &ReviewBadge) -> SharedString {
    format!("{}{}", badge.symbol, badge.number).into()
}

/// "Pull request #123: {title} ({state}) · Checks: 3 passed, 1 failed". While the forge cannot be reached the text
/// says that the state is the last known one.
pub fn tooltip_text(badge: &ReviewBadge, offline: bool) -> String {
    let unit = if badge.symbol == "!" {
        "Merge request"
    } else {
        "Pull request"
    };
    let mut text = match &badge.title {
        Some(title) => format!(
            "{unit} {}: {title} ({})",
            label_of(badge),
            badge.state.word()
        ),
        None => format!("{unit} {} ({})", label_of(badge), badge.state.word()),
    };
    if ci_state_of(badge).is_some()
        && let Some(checks) = badge.checks.describe()
    {
        text.push_str(" · Checks: ");
        text.push_str(&checks);
    }
    if offline {
        text.push_str(" · offline, showing the last known state");
    }
    text
}

/// The badge of `branch` and whether the forge is unreachable, from the tab's controller.
fn lookup(ctx: &TabCtx, branch: &FullName, cx: &App) -> Option<(ReviewBadge, bool)> {
    let controller = super::controller_of(ctx.id, cx)?;
    let controller = controller.read(cx);
    let badge = controller.badge(branch)?.clone();
    Some((badge, controller.offline()))
}

/// Whether something follows the badge in the row ("HEAD" or the ahead/behind text), which supplies the gap to the
/// row's end.
fn trailing_follows(ctx: &TabCtx, branch: &FullName, cx: &App) -> bool {
    let model = ctx.model.read(cx);
    let Some(refs) = &model.refs.value else {
        return false;
    };
    let Some(local) = refs.local.iter().find(|b| &b.full == branch) else {
        return false;
    };
    if local.is_head {
        return true;
    }
    let tracked = local
        .upstream
        .as_ref()
        .and_then(|upstream| upstream.tracking.as_ref())
        .and_then(|tracking| {
            refs.remotes
                .iter()
                .flat_map(|remote| &remote.branches)
                .find(|branch| &branch.full == tracking)
        });
    tracked
        .and_then(|remote| model.ahead_behind.get(&(local.target, remote.target)))
        .is_some_and(|ab| ab.ahead > 0 || ab.behind > 0)
}

fn open_in_browser(
    url: Option<String>,
) -> impl Fn(&gpui_kit::ClickEvent, &mut gpui_kit::Window, &mut App) {
    move |event, _, cx| {
        // The badge sits inside a clickable row or chip: only the badge reacts (a double-click must neither check
        // the branch out nor open the page twice).
        cx.stop_propagation();
        if event.click_count() > 1 {
            return;
        }
        if let Some(url) = &url {
            crate::dialogs::register::common::open_url(cx, url);
        }
    }
}

/// The `[PR badge][CI dot]` trailing content of a local-branch row in the side panel, before "HEAD". `None` when the
/// branch has no pull request in the controller's map.
pub fn branch_row_badge(ctx: &TabCtx, branch: &FullName, cx: &App) -> Option<AnyElement> {
    let (badge, offline) = lookup(ctx, branch, cx)?;
    let tokens = *cx.tokens();
    let name = branch.as_bstr().to_str_lossy().into_owned();
    let color = state_color(badge.state, &tokens);
    let margin_end = if trailing_follows(ctx, branch, cx) {
        metrics::ROW_MARGIN_END
    } else {
        metrics::ROW_MARGIN_END_LAST
    };
    let ci = ci_state_of(&badge);
    Some(
        div()
            .id(ids::row_badge(&name))
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .mr(dp(margin_end))
            .cursor_pointer()
            .delayed_tooltip(tooltip_text(&badge, offline))
            .on_click(open_in_browser(badge.url.clone()))
            .child(
                div()
                    .id(ids::pill(&name))
                    .h(dp(metrics::PILL_H))
                    .px(dp(metrics::PILL_PAD_H))
                    .flex()
                    .items_center()
                    .rounded(dp(metrics::PILL_RADIUS))
                    .bg(color.alpha(metrics::PILL_BG_ALPHA))
                    .text_caption()
                    .semibold()
                    .text_color(color)
                    .child(label_of(&badge))
                    .test_support(),
            )
            .when_some(ci, |row, ci| {
                row.child(
                    div()
                        .id(ids::row_ci_dot(&name))
                        .ml(dp(metrics::CI_DOT_MARGIN_START))
                        .size(dp(metrics::CI_DOT))
                        .rounded_full()
                        .bg(ci_color(ci, &tokens))
                        .test_support(),
                )
            })
            .test_support()
            .into_any_element(),
    )
}

/// The box of the PR segment. It is always the last one of its chip, so it rounds the chip's right end itself.
pub(super) fn segment_shell(bg: Hsla) -> gpui_kit::Div {
    round_chip_end(div(), ChipEnd::Right)
        .flex_none()
        .h(dp(metrics::SEGMENT_H))
        .px(dp(metrics::SEGMENT_PAD_H))
        .flex()
        .flex_row()
        .items_center()
        .bg(bg)
}

/// The PR segment at the end of a local-branch chip in the log: state colour background, an optional CI dot with a
/// 1 dp `on_primary` border, and "#123" in `on_primary`. `None` when the branch has no pull request.
pub fn chip_segment(ctx: &TabCtx, branch: &FullName, cx: &App) -> Option<AnyElement> {
    let (badge, offline) = lookup(ctx, branch, cx)?;
    let tokens = *cx.tokens();
    let name = branch.as_bstr().to_str_lossy().into_owned();
    let ci = ci_state_of(&badge);
    Some(
        segment_shell(state_color(badge.state, &tokens))
            .id(ids::segment(&name))
            .cursor_pointer()
            .delayed_tooltip(tooltip_text(&badge, offline))
            .on_click(open_in_browser(badge.url.clone()))
            .when_some(ci, |segment, ci| {
                segment.child(
                    div()
                        .id(ids::segment_ci_dot(&name))
                        .mr(dp(metrics::SEGMENT_DOT_GAP))
                        .size(dp(metrics::CI_DOT))
                        .rounded_full()
                        .bg(ci_color(ci, &tokens))
                        .border(dp(metrics::SEGMENT_DOT_BORDER))
                        .border_color(tokens.on_primary)
                        .test_support(),
                )
            })
            .child(
                div()
                    .text_caption()
                    .semibold()
                    .text_color(tokens.on_primary)
                    .child(label_of(&badge)),
            )
            .test_support()
            .into_any_element(),
    )
}
