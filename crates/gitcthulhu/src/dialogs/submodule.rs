//! The New submodule dialog (G10; 09 §3.9): repository URL, directory, an inline error banner and "Create
//! submodule". Unlike Gitnuro's, the dialog closes once the form validated and the operation started.

use gpui_kit::component::WindowExt as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Subscription, Window, div,
};

use super::IconBasedDialog;
use super::register::common::{self, FocusOnce};
use crate::backend::{RelaPath, validate};
use crate::icons::AppIcon;
use crate::metrics::dialogs::SUBMODULE_W;
use crate::metrics::dp;
use crate::tab::TabCtx;
use crate::theme::ActiveTokens as _;
use crate::widgets::field::{FieldEvent, OutlinedField};
use crate::widgets::observed;

/// Element ids of the submodule dialog.
pub mod ids {
    /// The dialog content.
    pub const SUBMODULE_DIALOG: &str = "submodule-dialog";
    /// The repository URL field.
    pub const SUBMODULE_URL: &str = "submodule-url";
    /// The directory field.
    pub const SUBMODULE_DIRECTORY: &str = "submodule-directory";
    /// The error banner.
    pub const SUBMODULE_ERROR: &str = "submodule-error";
}

/// Receives the validated `(url, directory)`.
type OnCreate = Box<dyn Fn(String, String, &mut Window, &mut App)>;

/// The content of an open submodule dialog.
struct SubmoduleDialog {
    tab: TabCtx,
    url: Entity<OutlinedField>,
    directory: Entity<OutlinedField>,
    /// The user typed in the directory field: it no longer follows the URL.
    directory_touched: bool,
    error: Option<SharedString>,
    on_create: OnCreate,
    focus: FocusOnce,
    _subscriptions: [Subscription; 2],
}

impl SubmoduleDialog {
    fn valid(&self, cx: &App) -> bool {
        !common::text_of(&self.url, cx).trim().is_empty()
            && !common::text_of(&self.directory, cx).trim().is_empty()
    }

    fn create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.valid(cx) {
            return;
        }
        let url = common::text_of(&self.url, cx).trim().to_owned();
        let directory = common::text_of(&self.directory, cx)
            .trim()
            .replace('\\', "/");
        let workdir = self
            .tab
            .backend
            .handle()
            .and_then(|handle| handle.workdir());
        if let Some(error) = validate::submodule_form_error(workdir.as_deref(), &url, &directory) {
            self.error = Some(error.into());
            cx.notify();
            return;
        }
        window.close_dialog(cx);
        (self.on_create)(url, directory, window, cx);
    }
}

impl Render for SubmoduleDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = *cx.tokens();
        self.focus.run(&self.url, false, window, cx);
        let this = cx.weak_entity();
        let content = observed(ids::SUBMODULE_DIALOG)
            .w(dp(SUBMODULE_W))
            .flex()
            .flex_col()
            .child(
                div()
                    .pb(dp(8.))
                    .child(common::form_label("Repository URL", &tokens)),
            )
            .child(self.url.clone())
            .child(
                div()
                    .pt(dp(16.))
                    .pb(dp(8.))
                    .child(common::form_label("Directory", &tokens)),
            )
            .child(self.directory.clone())
            .when_some(self.error.clone(), |col, error| {
                col.child(
                    observed(ids::SUBMODULE_ERROR).child(common::error_banner(error, &tokens)),
                )
            });
        IconBasedDialog::new(AppIcon::Submodule, "New submodule", "Create submodule")
            .subtitle("Create a new submodule from an existing repository")
            .primary_enabled(self.valid(cx))
            .content(content)
            .on_primary(move |window, cx| {
                this.update(cx, |d, cx| d.create(window, cx)).ok();
            })
    }
}

/// Opens the New submodule dialog.
pub fn open_new_submodule(window: &mut Window, cx: &mut App, tab: TabCtx) {
    let on_create: OnCreate = {
        let tab = tab.clone();
        Box::new(move |url, directory, window, cx| {
            tab.with_tab(window, cx, move |t, window, cx| {
                t.add_submodule(url, RelaPath::from(directory.as_str()), window, cx);
            });
        })
    };
    open_with(window, cx, tab, on_create);
}

/// Opens the dialog with a custom create callback (tests).
pub(crate) fn open_with(window: &mut Window, cx: &mut App, tab: TabCtx, on_create: OnCreate) {
    let url = common::new_field(ids::SUBMODULE_URL, None, "", window, cx);
    let directory = common::new_field(ids::SUBMODULE_DIRECTORY, None, "", window, cx);
    let view = cx.new(|cx| {
        let on_url = cx.subscribe_in(
            &url,
            window,
            |this: &mut SubmoduleDialog, _, event: &FieldEvent, window, cx| {
                if *event != FieldEvent::Changed {
                    return;
                }
                this.error = None;
                if !this.directory_touched {
                    let name = validate::repo_name_from_url(&common::text_of(&this.url, cx));
                    this.directory
                        .update(cx, |field, cx| field.set_text(&name, window, cx));
                }
                cx.notify();
            },
        );
        let on_directory = cx.subscribe_in(
            &directory,
            window,
            |this: &mut SubmoduleDialog, _, event: &FieldEvent, window, cx| match event {
                FieldEvent::Changed => {
                    this.directory_touched = true;
                    this.error = None;
                    cx.notify();
                }
                FieldEvent::Enter => this.create(window, cx),
            },
        );
        SubmoduleDialog {
            tab,
            url,
            directory,
            directory_touched: false,
            error: None,
            on_create,
            focus: FocusOnce::default(),
            _subscriptions: [on_url, on_directory],
        }
    });
    common::open_view(window, cx, SUBMODULE_W, 420., view);
}
