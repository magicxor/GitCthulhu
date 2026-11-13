//! Progress indicators (Gitnuro 09 §0.3): the 4 dp linear bar (indeterminate or determinate) and the 40 dp
//! circular spinner.

use std::time::Duration;

use gpui_kit::component::{Icon, IconName};
use gpui_kit::{
    Animation, AnimationExt as _, App, Hsla, IntoElement, ParentElement as _, RenderOnce,
    Styled as _, Window, div, ease_in_out, percentage, relative,
};

use crate::metrics::dp;
use crate::theme::ActiveTokens as _;

/// Linear progress bar, 4 dp high. Without a value it is indeterminate (a bar sweeping across the track).
#[derive(IntoElement)]
pub struct LinearProgress {
    value: Option<f32>,
    color: Option<Hsla>,
    width: Option<f32>,
}

impl LinearProgress {
    /// An indeterminate bar that fills its parent's width.
    pub fn new() -> Self {
        Self {
            value: None,
            color: None,
            width: None,
        }
    }

    /// Determinate progress in `0.0..=1.0`.
    pub fn value(mut self, value: f32) -> Self {
        self.value = Some(value.clamp(0.0, 1.0));
        self
    }

    /// Bar colour (default `primary_variant`; the processing screen uses `secondary`).
    pub fn color(mut self, color: Hsla) -> Self {
        self.color = Some(color);
        self
    }

    /// Fixed width in dp.
    pub fn width(mut self, width_dp: f32) -> Self {
        self.width = Some(width_dp);
        self
    }
}

impl Default for LinearProgress {
    fn default() -> Self {
        Self::new()
    }
}

impl RenderOnce for LinearProgress {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let tokens = *cx.tokens();
        let color = self.color.unwrap_or(tokens.primary_variant);
        let track = div()
            .relative()
            .h(dp(4.))
            .overflow_hidden()
            .bg(color.alpha(0.24));
        let track = match self.width {
            Some(w) => track.w(dp(w)),
            None => track.w_full(),
        };
        match self.value {
            Some(value) => track
                .child(div().h_full().w(relative(value)).bg(color))
                .into_any_element(),
            None => track
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .h_full()
                        .w(relative(0.4))
                        .bg(color)
                        .with_animation(
                            "linear-progress-sweep",
                            Animation::new(Duration::from_millis(1400))
                                .repeat()
                                .with_easing(ease_in_out),
                            |bar, delta| bar.left(relative(delta * 1.4 - 0.4)),
                        ),
                )
                .into_any_element(),
        }
    }
}

/// Circular indeterminate progress, 40 dp.
#[derive(IntoElement)]
pub struct CircularProgress {
    color: Option<Hsla>,
}

impl CircularProgress {
    /// A spinner in `primary_variant`.
    pub fn new() -> Self {
        Self { color: None }
    }

    /// Spinner colour.
    pub fn color(mut self, color: Hsla) -> Self {
        self.color = Some(color);
        self
    }
}

impl Default for CircularProgress {
    fn default() -> Self {
        Self::new()
    }
}

impl RenderOnce for CircularProgress {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let color = self.color.unwrap_or(cx.tokens().primary_variant);
        Icon::new(IconName::LoaderCircle)
            .size(dp(40.))
            .text_color(color)
            .with_animation(
                "circular-progress-spin",
                Animation::new(Duration::from_millis(1000)).repeat(),
                |icon, delta| icon.rotate(percentage(delta)),
            )
    }
}
