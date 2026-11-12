//! Pieces shared by the G10 dialogs: platform side effects behind an injectable hook (the gpui test platform panics
//! on clipboard, URL and folder-picker calls), the field helpers and the small widgets Gitnuro's dialogs repeat.

use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::component::{Icon, IconName, WindowExt as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, Div, ElementId, Entity, Global,
    InteractiveElement as _, IntoElement, ParentElement as _, PathPromptOptions, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, TestSupportExt as _,
    Window, div,
};

use crate::dialogs::{DialogFrame, DialogSpec, IconBasedDialog, SingleFieldConfig};
use crate::metrics::{self, dp};
use crate::theme::{TextStyleExt as _, Tokens};
use crate::widgets::field::{FieldEvent, OutlinedField};
use crate::widgets::with_layer;

/// Replacements for the platform side effects of the dialogs. Unset entries use the real platform call; tests
/// install a `DialogHooks` global that records the calls.
#[derive(Default, Clone)]
pub struct DialogHooks {
    /// Puts text on the clipboard.
    pub copy: Option<Rc<dyn Fn(&str)>>,
    /// Opens a URL in the browser.
    pub open_url: Option<Rc<dyn Fn(&str)>>,
    /// Chooses a directory; `None` = the user cancelled.
    pub pick_folder: Option<Rc<dyn Fn() -> Option<PathBuf>>>,
}

impl Global for DialogHooks {}

fn hooks(cx: &App) -> DialogHooks {
    cx.try_global::<DialogHooks>().cloned().unwrap_or_default()
}

/// Copies `text` to the clipboard.
pub fn copy_text(cx: &mut App, text: &str) {
    match hooks(cx).copy {
        Some(copy) => copy(text),
        None => cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned())),
    }
}

/// Opens `url` in the default browser.
pub fn open_url(cx: &mut App, url: &str) {
    match hooks(cx).open_url {
        Some(open) => open(url),
        None => cx.open_url(url),
    }
}

/// Asks for a directory with the OS picker and calls `on_pick` with it (not at all when cancelled).
pub fn pick_folder(
    window: &mut Window,
    cx: &mut App,
    on_pick: impl FnOnce(PathBuf, &mut Window, &mut App) + 'static,
) {
    if let Some(pick) = hooks(cx).pick_folder {
        if let Some(path) = pick() {
            on_pick(path, window, cx);
        }
        return;
    }
    let paths = cx.prompt_for_paths(PathPromptOptions {
        files: false,
        directories: true,
        multiple: false,
        prompt: None,
    });
    window
        .spawn(cx, async move |cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            cx.update(|window, cx| on_pick(path, window, cx)).ok();
        })
        .detach();
}

/// Opens `view` as a dialog of `content_w` dp content width (the frame adds its 16 dp padding on both sides).
pub fn open_view<V: Render + 'static>(
    window: &mut Window,
    cx: &mut App,
    content_w: f32,
    est_height: f32,
    view: Entity<V>,
) {
    DialogFrame::open(
        window,
        cx,
        DialogSpec {
            width: content_w + 2. * metrics::DIALOG_PAD,
            est_height,
        },
        move |_, _| view.clone().into_any_element(),
    );
}

/// The current text of a field.
pub fn text_of(field: &Entity<OutlinedField>, cx: &App) -> String {
    field.read(cx).text(cx).to_string()
}

/// Focuses a field once, after the kit focused the dialog shell on its first draw (two deferrals), optionally
/// selecting all its text.
#[derive(Default)]
pub struct FocusOnce(bool);

impl FocusOnce {
    /// Call from `render`; focuses `field` the first time only.
    pub fn run(
        &mut self,
        field: &Entity<OutlinedField>,
        select_all: bool,
        window: &mut Window,
        cx: &mut App,
    ) {
        if self.0 {
            return;
        }
        self.0 = true;
        let field = field.clone();
        window.defer(cx, move |window, cx| {
            window.defer(cx, move |window, cx| {
                field.update(cx, |f, cx| {
                    f.focus(window, cx);
                    if select_all {
                        f.select_all(window, cx);
                    } else {
                        f.caret_to_end(cx);
                    }
                });
            });
        });
    }
}

/// A body 1 label above a field (Gitnuro `Text(label, body1)`).
pub fn form_label(text: impl Into<SharedString>, tokens: &Tokens) -> Div {
    div()
        .text_body1()
        .text_color(tokens.on_background)
        .child(text.into())
}

/// The red inline error banner of the clone and submodule dialogs (09 §3.0).
pub fn error_banner(message: impl Into<SharedString>, tokens: &Tokens) -> Div {
    div().w_full().py(dp(4.)).child(
        div()
            .w_full()
            .rounded(dp(metrics::RADIUS))
            .bg(tokens.error)
            .px(dp(8.))
            .py(dp(4.))
            .text_body1()
            .text_color(tokens.on_primary)
            .child(message.into()),
    )
}

/// A button with a custom fill (Gitnuro `PrimaryButton` geometry): the destructive confirm button uses `error`.
pub fn filled_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    bg: gpui_kit::Hsla,
    fg: gpui_kit::Hsla,
    enabled: bool,
    tokens: &Tokens,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> AnyElement {
    let tokens = *tokens;
    let (bg, fg) = if enabled {
        (bg, fg)
    } else {
        (bg.opacity(0.5), fg.opacity(0.5))
    };
    div()
        .id(id)
        .flex_none()
        .px(dp(metrics::PRIMARY_BUTTON_PAD_H))
        .py(dp(metrics::PRIMARY_BUTTON_PAD_V))
        .rounded(dp(metrics::RADIUS))
        .bg(bg)
        .text_color(fg)
        .text_body1()
        .cursor_pointer()
        .when(enabled, |b| {
            b.hover(move |s| s.bg(with_layer(bg, tokens.hover_layer())))
                .active(move |s| s.bg(with_layer(bg, tokens.press_layer())))
                .on_click(move |_, window, cx| on_click(window, cx))
        })
        .child(label.into())
        .test_support()
        .into_any_element()
}

/// The 20 dp show / hide toggle of password fields (09 §3.15): `eye` while hidden, `eye-off` while shown.
pub fn eye_toggle(
    id: impl Into<ElementId>,
    shown: bool,
    tokens: &Tokens,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> AnyElement {
    let tokens = *tokens;
    div()
        .id(id)
        .flex_none()
        .size(dp(metrics::dialogs::EYE_BUTTON))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .cursor_pointer()
        .hover(move |s| s.bg(tokens.hover_layer()))
        .on_click(move |_, window, cx| on_click(window, cx))
        .child(
            Icon::new(if shown {
                IconName::EyeOff
            } else {
                IconName::Eye
            })
            .size(dp(metrics::ICON_SM + 2.))
            .text_color(tokens.on_background),
        )
        .test_support()
        .into_any_element()
}

/// Creates an entity holding a new [`OutlinedField`].
pub fn new_field(
    id: impl Into<SharedString>,
    hint: Option<&str>,
    initial: &str,
    window: &mut Window,
    cx: &mut App,
) -> Entity<OutlinedField> {
    let id = id.into();
    let hint = hint.map(|h| SharedString::from(h.to_owned()));
    let initial = initial.to_owned();
    cx.new(|cx| {
        let mut field = OutlinedField::new(id, window, cx);
        if let Some(hint) = hint {
            field = field.hint(hint, window, cx);
        }
        field.set_text(&initial, window, cx);
        field
    })
}

/// A single-field dialog like [`SingleTextFieldDialog`](crate::dialogs::SingleTextFieldDialog) whose prefilled text
/// is fully selected when the dialog opens (Rename branch, Choose another local name; 09 §3.3).
pub struct PrefilledFieldDialog {
    config: SingleFieldConfig,
    field: Entity<OutlinedField>,
    focus: FocusOnce,
    _subscription: Subscription,
}

impl PrefilledFieldDialog {
    /// Opens the dialog; `config.initial_text` is the prefilled text.
    pub fn open(window: &mut Window, cx: &mut App, config: SingleFieldConfig) {
        let hint = config.hint.clone();
        let initial = config.initial_text.clone();
        let field = new_field("single-field-input", hint.as_deref(), &initial, window, cx);
        let view = cx.new(|cx| {
            let subscription = cx.subscribe_in(
                &field,
                window,
                |this: &mut PrefilledFieldDialog, _, event: &FieldEvent, window, cx| match event {
                    FieldEvent::Changed => cx.notify(),
                    FieldEvent::Enter => this.accept(window, cx),
                },
            );
            PrefilledFieldDialog {
                config,
                field,
                focus: FocusOnce::default(),
                _subscription: subscription,
            }
        });
        open_view(window, cx, metrics::SINGLE_FIELD_W, 260., view);
    }

    fn accept(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = text_of(&self.field, cx);
        if !(self.config.is_valid)(&text) {
            return;
        }
        window.close_dialog(cx);
        (self.config.on_accept)(text, window, cx);
    }
}

impl Render for PrefilledFieldDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.focus.run(&self.field, true, window, cx);
        let valid = (self.config.is_valid)(&text_of(&self.field, cx));
        let this = cx.weak_entity();
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
