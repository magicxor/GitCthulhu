//! The New / Edit remote dialog (G10; 09 §3.8): a 600 dp column with the remote name (new remotes only), the fetch
//! URL and the push URL. While the push URL equals the fetch URL, editing the fetch URL also edits the push URL.

use gpui_kit::component::WindowExt as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _,
    Subscription, Window, div,
};

use super::register::common::{self, FocusOnce};
use crate::metrics::dialogs::{REMOTE_BUTTONS_TOP, REMOTE_W};
use crate::metrics::dp;
use crate::tab::TabCtx;
use crate::theme::{ActiveTokens as _, TextStyleExt as _};
use crate::widgets::field::{FieldEvent, OutlinedField};
use crate::widgets::{PrimaryButton, observed};

/// Element ids of the remote dialog.
pub mod ids {
    /// The dialog content.
    pub const REMOTE_DIALOG: &str = "remote-dialog";
    /// The remote name field (new remotes only).
    pub const REMOTE_NAME: &str = "remote-name";
    /// The fetch URL field.
    pub const REMOTE_FETCH: &str = "remote-fetch-url";
    /// The push URL field.
    pub const REMOTE_PUSH: &str = "remote-push-url";
}

/// What the dialog edits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteTarget {
    /// A new remote.
    New,
    /// The remote of this name.
    Edit(String),
}

/// What Save sends: the (trimmed) name, fetch URL and the push URL (`None` when empty or equal to the fetch URL).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteForm {
    /// Remote name.
    pub name: String,
    /// Fetch URL.
    pub url: String,
    /// Separate push URL.
    pub push_url: Option<String>,
}

impl RemoteForm {
    /// Builds the form from the raw field texts.
    pub fn from_fields(name: &str, fetch: &str, push: &str) -> Self {
        let url = fetch.trim().to_owned();
        let push = push.trim();
        Self {
            name: name.trim().to_owned(),
            push_url: (!push.is_empty() && push != url).then(|| push.to_owned()),
            url,
        }
    }
}

/// Receives the saved form after the dialog closed.
pub(crate) type OnSave = Box<dyn Fn(RemoteForm, &mut Window, &mut App)>;

/// The content of an open remote dialog.
struct RemoteDialog {
    target: RemoteTarget,
    name: Entity<OutlinedField>,
    fetch: Entity<OutlinedField>,
    push: Entity<OutlinedField>,
    /// The fetch text before the last edit (push follows fetch while it equals this).
    last_fetch: String,
    on_save: OnSave,
    focus: FocusOnce,
    _subscription: Subscription,
}

impl RemoteDialog {
    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let form = RemoteForm::from_fields(
            &common::text_of(&self.name, cx),
            &common::text_of(&self.fetch, cx),
            &common::text_of(&self.push, cx),
        );
        window.close_dialog(cx);
        (self.on_save)(form, window, cx);
    }
}

impl Render for RemoteDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = *cx.tokens();
        let first = match self.target {
            RemoteTarget::New => &self.name,
            RemoteTarget::Edit(_) => &self.fetch,
        };
        self.focus.run(first, false, window, cx);
        let title = match &self.target {
            RemoteTarget::New => "New remote".to_owned(),
            RemoteTarget::Edit(name) => format!("Edit remote \"{name}\""),
        };
        let this = cx.weak_entity();
        let block = |label: &str, field: &Entity<OutlinedField>| {
            div()
                .w_full()
                .flex()
                .flex_col()
                .child(
                    div()
                        .pt(dp(8.))
                        .child(common::form_label(label.to_owned(), &tokens)),
                )
                .child(div().py(dp(8.)).child(field.clone()))
        };
        observed(ids::REMOTE_DIALOG)
            .w(dp(REMOTE_W))
            .flex()
            .flex_col()
            // The dialog frame pads 16 dp at the bottom where Gitnuro's dialog pads 8 dp.
            .mb(dp(-8.))
            .child(div().pl(dp(8.)).pb(dp(8.)).text_h3().child(title))
            .child(
                div()
                    .px(dp(8.))
                    .pb(dp(8.))
                    .flex()
                    .flex_col()
                    .when(self.target == RemoteTarget::New, |col| {
                        col.child(block("Remote name", &self.name))
                    })
                    .child(block("Fetch URL", &self.fetch))
                    .child(block("Push URL", &self.push)),
            )
            .child(
                div()
                    .pt(dp(REMOTE_BUTTONS_TOP))
                    .flex()
                    .flex_row()
                    .justify_end()
                    .child(
                        div().pr(dp(8.)).child(
                            PrimaryButton::new("dialog-cancel", "Cancel")
                                .cancel()
                                .on_click(|window, cx| window.close_dialog(cx)),
                        ),
                    )
                    .child(
                        PrimaryButton::new("dialog-primary", "Save changes").on_click(
                            move |window, cx| {
                                this.update(cx, |d, cx| d.save(window, cx)).ok();
                            },
                        ),
                    ),
            )
    }
}

pub(crate) fn open_dialog(
    window: &mut Window,
    cx: &mut App,
    target: RemoteTarget,
    fetch: &str,
    push: &str,
    on_save: OnSave,
) {
    let name_field = common::new_field(ids::REMOTE_NAME, None, "", window, cx);
    let fetch_field = common::new_field(ids::REMOTE_FETCH, None, fetch, window, cx);
    let push_field = common::new_field(ids::REMOTE_PUSH, None, push, window, cx);
    let last_fetch = fetch.to_owned();
    let view = cx.new(|cx| {
        let subscription = cx.subscribe_in(
            &fetch_field,
            window,
            |this: &mut RemoteDialog, _, event: &FieldEvent, window, cx| {
                if *event != FieldEvent::Changed {
                    return;
                }
                let fetch = common::text_of(&this.fetch, cx);
                let push = common::text_of(&this.push, cx);
                if push == this.last_fetch {
                    this.push
                        .update(cx, |field, cx| field.set_text(&fetch, window, cx));
                }
                this.last_fetch = fetch;
                cx.notify();
            },
        );
        RemoteDialog {
            target,
            name: name_field,
            fetch: fetch_field,
            push: push_field,
            last_fetch,
            on_save,
            focus: FocusOnce::default(),
            _subscription: subscription,
        }
    });
    common::open_view(window, cx, REMOTE_W, 420., view);
}

/// Opens the New remote dialog.
pub fn open_new_remote(window: &mut Window, cx: &mut App, tab: TabCtx) {
    open_dialog(
        window,
        cx,
        RemoteTarget::New,
        "",
        "",
        Box::new(move |form, window, cx| {
            tab.with_tab(window, cx, move |t, window, cx| {
                t.add_remote(form.name, form.url, form.push_url, window, cx);
            });
        }),
    );
}

/// Opens the Edit remote dialog of remote `name`.
pub fn open_edit_remote(window: &mut Window, cx: &mut App, tab: TabCtx, name: String) {
    let (fetch, push) = tab
        .model
        .read(cx)
        .refs
        .value
        .as_ref()
        .and_then(|refs| refs.remotes.iter().find(|r| r.name == name))
        .map(|remote| {
            let fetch = remote.fetch_url.clone().unwrap_or_default();
            let push = remote.push_url.clone().unwrap_or_else(|| fetch.clone());
            (fetch, push)
        })
        .unwrap_or_default();
    let edited = name.clone();
    open_dialog(
        window,
        cx,
        RemoteTarget::Edit(name),
        &fetch,
        &push,
        Box::new(move |form, window, cx| {
            let name = edited.clone();
            tab.with_tab(window, cx, move |t, window, cx| {
                t.edit_remote(name, form.url, form.push_url, window, cx);
            });
        }),
    );
}
