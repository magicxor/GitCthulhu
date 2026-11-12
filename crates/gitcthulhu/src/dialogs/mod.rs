//! Dialog hosting and the shared dialog templates (spec 21 §2.13.4, Gitnuro 09 §3.0).
//!
//! The kit's dialog host draws a 1 px border, offsets the dialog to a tenth of the viewport height, animates and
//! closes on Escape. [`DialogFrame`] neutralises that chrome (no padding, border, background, shadow or entrance)
//! and draws Gitnuro's frame itself: 1 dp `dialog_border`, 8 dp radius, `surface` background, 16 dp padding. The
//! dialog is centred vertically from an estimated height; stacked dialogs keep the kit's 16 px offset
//! (deviation 33).

use std::rc::Rc;

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::dialog::DialogEntrance;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render,
    RenderOnce, SharedString, Styled as _, Subscription, Window, div, px, transparent_black,
};

use crate::icons::{AppIcon, icon};
use crate::ids;
use crate::metrics::{self, dp, dpx};
use crate::theme::{ActiveTokens as _, TextStyleExt as _};
use crate::widgets::field::{FieldEvent, OutlinedField};
use crate::widgets::{PrimaryButton, observed};

pub mod about;
pub mod author;
pub mod branch;
pub mod choice;
pub mod clone;
pub mod confirm;
pub mod credentials;
pub mod error;
pub mod pull_request;
pub mod quick_actions;
pub mod register;
pub mod remote;
pub mod reset;
pub mod settings;
pub mod signoff;
pub mod stash;
pub mod submodule;
pub mod tag;
pub mod worktree;

/// Width of the dialog border in dp (09 §3.0 `MaterialDialog`).
const DIALOG_BORDER: f32 = 1.;

/// Size hints of a dialog.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DialogSpec {
    /// Total dialog width in dp, including the 16 dp padding on both sides.
    pub width: f32,
    /// Estimated height in dp, used to centre the dialog vertically.
    pub est_height: f32,
}

/// The vertical offset (px) that centres a dialog of `est_height_px` in a viewport `viewport_h` high, but never
/// less than 16 px from the top.
pub fn centered_margin_top(viewport_h: f32, est_height_px: f32) -> f32 {
    ((viewport_h - est_height_px) / 2.).max(16.)
}

/// Gitnuro's `MaterialDialog` chrome around `content`.
#[derive(IntoElement)]
pub struct DialogFrame {
    content: AnyElement,
}

impl DialogFrame {
    /// Wraps `content` in the dialog frame.
    pub fn new(content: impl IntoElement) -> Self {
        Self {
            content: content.into_any_element(),
        }
    }

    /// Opens a dialog layer of `spec.width` whose content is `build` wrapped in the frame. `build` may run again
    /// whenever the kit rebuilds the layer, so it must be cheap and capture entities created before the call.
    /// Escape closes the dialog; clicking outside does nothing.
    pub fn open(
        window: &mut Window,
        cx: &mut App,
        spec: DialogSpec,
        build: impl Fn(&mut Window, &mut App) -> AnyElement + 'static,
    ) {
        let build = Rc::new(build);
        window.open_dialog(cx, move |dialog, window, _| {
            let build = build.clone();
            let margin = centered_margin_top(
                window.viewport_size().height.as_f32(),
                dpx(spec.est_height, window).as_f32(),
            );
            dialog
                .entrance(DialogEntrance::None)
                .close_button(false)
                .overlay_closable(false)
                // Enter is handled by the dialog's own field and buttons, never by the kit's default OK.
                .on_ok(|_, _, _| false)
                .p_0()
                .border_0()
                .bg(transparent_black())
                .shadow_none()
                .w(dpx(spec.width, window))
                .margin_top(px(margin))
                .content(move |content, window, cx| {
                    content.child(DialogFrame::new(build(window, cx)))
                })
        });
    }
}

impl RenderOnce for DialogFrame {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let tokens = *cx.tokens();
        observed(ids::DIALOG_FRAME)
            .w_full()
            .border(dp(DIALOG_BORDER))
            .border_color(tokens.dialog_border())
            .rounded(dp(metrics::RADIUS_LG))
            .overflow_hidden()
            .bg(tokens.surface)
            // Compose draws the border over the 16 dp padding; a GPUI border takes layout space, so the padding
            // shrinks by the border width and the content keeps Gitnuro's `width - 32` (a 332 dp single-field
            // dialog holds exactly its 300 dp field).
            .p(dp(metrics::DIALOG_PAD - DIALOG_BORDER))
            .text_color(tokens.on_background)
            .child(self.content)
    }
}

/// Gitnuro `IconBasedDialog`: icon, title, subtitle, content and a button row (Cancel, primary).
///
/// The width is that of the surrounding frame; set [`DialogSpec::width`] to the content width plus 32.
#[derive(IntoElement)]
pub struct IconBasedDialog {
    role: AppIcon,
    title: SharedString,
    subtitle: Option<SharedString>,
    content: Option<AnyElement>,
    primary_label: SharedString,
    primary_enabled: bool,
    on_primary: Option<Rc<dyn Fn(&mut Window, &mut App)>>,
    show_cancel: bool,
    cancel_label: SharedString,
    on_cancel: Option<Rc<dyn Fn(&mut Window, &mut App)>>,
}

impl IconBasedDialog {
    /// A dialog with a 64 dp icon, a title and a primary button label.
    pub fn new(
        role: AppIcon,
        title: impl Into<SharedString>,
        primary_label: impl Into<SharedString>,
    ) -> Self {
        Self {
            role,
            title: title.into(),
            subtitle: None,
            content: None,
            primary_label: primary_label.into(),
            primary_enabled: true,
            on_primary: None,
            show_cancel: true,
            cancel_label: "Cancel".into(),
            on_cancel: None,
        }
    }

    /// Sets the centred explanation under the title.
    pub fn subtitle(mut self, subtitle: impl Into<SharedString>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }

    /// Sets the dialog body.
    pub fn content(mut self, content: impl IntoElement) -> Self {
        self.content = Some(content.into_any_element());
        self
    }

    /// Enables or disables the primary button.
    pub fn primary_enabled(mut self, enabled: bool) -> Self {
        self.primary_enabled = enabled;
        self
    }

    /// Sets the primary action. The dialog is not closed automatically; call `window.close_dialog(cx)`.
    pub fn on_primary(mut self, f: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_primary = Some(Rc::new(f));
        self
    }

    /// Hides the Cancel button.
    pub fn without_cancel(mut self) -> Self {
        self.show_cancel = false;
        self
    }

    /// Changes the Cancel label.
    pub fn cancel_label(mut self, label: impl Into<SharedString>) -> Self {
        self.cancel_label = label.into();
        self
    }

    /// Runs `f` after Cancel closed the dialog.
    pub fn on_cancel(mut self, f: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_cancel = Some(Rc::new(f));
        self
    }
}

impl RenderOnce for IconBasedDialog {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let tokens = *cx.tokens();
        let on_primary = self.on_primary;
        let on_cancel = self.on_cancel;
        div()
            .w_full()
            .flex()
            .flex_col()
            .items_center()
            .child(div().pb(dp(16.)).child(icon(
                self.role,
                metrics::DIALOG_ICON,
                tokens.on_background,
            )))
            .child(
                div()
                    .pb(dp(8.))
                    .text_body1()
                    .semibold()
                    .text_color(tokens.on_background)
                    .child(self.title),
            )
            .when_some(self.subtitle, |col, subtitle| {
                col.child(
                    div()
                        .pb(dp(16.))
                        .w_full()
                        .flex()
                        .justify_center()
                        .text_center()
                        .text_body2()
                        .text_color(tokens.on_background)
                        .child(subtitle),
                )
            })
            .children(self.content)
            .child(
                div()
                    .w_full()
                    .pt(dp(16.))
                    .flex()
                    .flex_row()
                    .justify_end()
                    .when(self.show_cancel, |row| {
                        row.child(
                            div().pr(dp(8.)).child(
                                PrimaryButton::new("dialog-cancel", self.cancel_label)
                                    .cancel()
                                    .on_click(move |window, cx| {
                                        window.close_dialog(cx);
                                        if let Some(f) = &on_cancel {
                                            f(window, cx);
                                        }
                                    }),
                            ),
                        )
                    })
                    .child(
                        PrimaryButton::new("dialog-primary", self.primary_label)
                            .enabled(self.primary_enabled)
                            .when_some(on_primary, |b, f| {
                                b.on_click(move |window, cx| f(window, cx))
                            }),
                    ),
            )
    }
}

/// What a [`SingleTextFieldDialog`] shows and does.
#[derive(Clone)]
pub struct SingleFieldConfig {
    pub icon: AppIcon,
    pub title: SharedString,
    pub subtitle: Option<SharedString>,
    pub primary_label: SharedString,
    pub hint: Option<SharedString>,
    pub initial_text: String,
    /// The primary button is enabled iff this returns `true` for the current text (default: not blank).
    pub is_valid: Rc<dyn Fn(&str) -> bool>,
    /// Receives the accepted text after the dialog was closed.
    pub on_accept: Rc<dyn Fn(String, &mut Window, &mut App)>,
}

impl SingleFieldConfig {
    /// A configuration whose primary button needs non-blank text.
    pub fn new(
        icon: AppIcon,
        title: impl Into<SharedString>,
        primary_label: impl Into<SharedString>,
        on_accept: impl Fn(String, &mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            icon,
            title: title.into(),
            subtitle: None,
            primary_label: primary_label.into(),
            hint: None,
            initial_text: String::new(),
            is_valid: Rc::new(|text| !text.trim().is_empty()),
            on_accept: Rc::new(on_accept),
        }
    }
}

/// Gitnuro `SingleTextFieldDialog`: an `IconBasedDialog` with one 300 dp auto-focused text field; Enter triggers
/// the primary action while it is enabled. The dialog is 332 dp wide.
pub struct SingleTextFieldDialog {
    config: SingleFieldConfig,
    field: Entity<OutlinedField>,
    focused: bool,
    _subscription: Subscription,
}

impl SingleTextFieldDialog {
    /// Total dialog width: 300 dp field plus 2 x 16 dp padding.
    pub const WIDTH: f32 = metrics::SINGLE_FIELD_W + 2. * metrics::DIALOG_PAD;

    /// Creates the dialog view (needs a window for the text input).
    pub fn new(config: SingleFieldConfig, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let hint = config.hint.clone();
        let initial = config.initial_text.clone();
        let field = cx.new(|cx| {
            let mut field = OutlinedField::new("single-field-input", window, cx);
            if let Some(hint) = hint {
                field = field.hint(hint, window, cx);
            }
            field.set_text(&initial, window, cx);
            field
        });
        let subscription = cx.subscribe_in(
            &field,
            window,
            |this, _, event: &FieldEvent, window, cx| match event {
                FieldEvent::Changed => cx.notify(),
                FieldEvent::Enter => this.accept(window, cx),
            },
        );
        Self {
            config,
            field,
            focused: false,
            _subscription: subscription,
        }
    }

    /// Opens the dialog in the window.
    pub fn open(window: &mut Window, cx: &mut App, config: SingleFieldConfig) {
        let view = cx.new(|cx| SingleTextFieldDialog::new(config, window, cx));
        DialogFrame::open(
            window,
            cx,
            DialogSpec {
                width: Self::WIDTH,
                est_height: 260.,
            },
            move |_, _| view.clone().into_any_element(),
        );
    }

    fn text(&self, cx: &App) -> String {
        self.field.read(cx).text(cx).to_string()
    }

    fn accept(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.text(cx);
        if !(self.config.is_valid)(&text) {
            return;
        }
        window.close_dialog(cx);
        (self.config.on_accept)(text, window, cx);
    }
}

impl Render for SingleTextFieldDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.focused {
            self.focused = true;
            let field = self.field.clone();
            // The kit dialog focuses its own handle after the layer is first drawn; focus the field after that.
            window.defer(cx, move |window, cx| {
                window.defer(cx, move |window, cx| {
                    field.update(cx, |f, cx| f.focus(window, cx));
                });
            });
        }
        let valid = (self.config.is_valid)(&self.text(cx));
        let this = cx.entity().downgrade();
        IconBasedDialog::new(
            self.config.icon,
            self.config.title.clone(),
            self.config.primary_label.clone(),
        )
        .when_some(self.config.subtitle.clone(), |d, s| d.subtitle(s))
        .primary_enabled(valid)
        .on_primary(move |window, cx| {
            this.update(cx, |dialog, cx| dialog.accept(window, cx)).ok();
        })
        .content(
            div()
                .w(dp(metrics::SINGLE_FIELD_W))
                .child(self.field.clone()),
        )
    }
}

#[cfg(test)]
mod tests;
