//! Credential prompts per `PromptKind` (G10; 09 §3.15, §3.16, spec 21 §2.8, 20 §6.2).
//!
//! Git asks for a user name and a password in two separate prompts, so each is its own dialog. SSH passphrases,
//! passwords and PINs show one masked field with a show / hide toggle, an unknown host key shows the fingerprint
//! with **No** / **Yes**, and anything else shows Git's own prompt text. A repeated prompt (`attempt > 1`) shows
//! the red retry banner until the first keystroke. The dialog polls the request's cancel token every 200 ms and
//! closes itself when it fired. Escape and Cancel answer `Cancel`.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::SyncSender;
use std::time::Duration;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Subscription, Task, Window, div,
};

use super::register::common::{self, FocusOnce};
use super::{IconBasedDialog, LayerId, close_layer_or_top};
use crate::backend::{CredentialRequest, CredentialResponse, PromptKind, Sensitive};
use crate::icons::AppIcon;
use crate::metrics::dialogs::{CREDENTIAL_W, HOST_KEY_W};
use crate::metrics::{self, dp};
use crate::theme::{ActiveTokens as _, TextStyleExt as _};
use crate::widgets::CheckboxText;
use crate::widgets::field::{FieldEvent, OutlinedField};

/// How often the dialog checks the request's cancel token.
const CANCEL_POLL: Duration = Duration::from_millis(200);
/// End padding of a field with a trailing icon (09 §0.3 `AdjustableOutlinedTextField`).
const EYE_END_PAD: f32 = 4.;

/// Element ids of the credential dialogs.
pub mod ids {
    /// The dialog content.
    pub const CREDENTIAL_DIALOG: &str = "credential-dialog";
    /// The input field.
    pub const CREDENTIAL_FIELD: &str = "credential-field";
    /// The show / hide toggle of a masked field.
    pub const CREDENTIAL_EYE: &str = "credential-eye";
    /// The retry banner.
    pub const CREDENTIAL_RETRY: &str = "credential-retry";
    /// The host-key fingerprint line.
    pub const CREDENTIAL_FINGERPRINT: &str = "credential-fingerprint";
    /// The "Use {forge} account {user}" checkbox of an HTTPS prompt (X4).
    pub const CREDENTIAL_USE_ACCOUNT: &str = "credential-use-account";
}

/// What a prompt looks like: icon, texts and whether the field is masked.
#[derive(Clone, Debug, PartialEq)]
pub struct PromptTexts {
    /// 64 dp icon.
    pub icon: AppIcon,
    /// Bold title.
    pub title: SharedString,
    /// Explanation under the title.
    pub subtitle: SharedString,
    /// Hint of the field; `None` = the prompt has no field (host key).
    pub hint: Option<&'static str>,
    /// The field hides its text until the eye toggle is pressed.
    pub masked: bool,
    /// Label of the primary button.
    pub primary: &'static str,
    /// Label of the cancel button.
    pub cancel: &'static str,
    /// The retry banner text.
    pub retry: &'static str,
}

/// The texts of `request` (pure, so tests can check every kind).
pub fn prompt_texts(request: &CredentialRequest) -> PromptTexts {
    let operation = request.operation.trim();
    let with_operation = |text: String| -> SharedString {
        if operation.is_empty() {
            text.into()
        } else {
            format!("{text}\n{operation}").into()
        }
    };
    let password_retry = "Invalid password, please try again";
    match &request.kind {
        PromptKind::HttpsUsername { url } => PromptTexts {
            icon: AppIcon::Lock,
            title: "Introduce your remote server credentials".into(),
            subtitle: with_operation(format!("Username for {url}")),
            hint: Some("Username"),
            masked: false,
            primary: "Continue",
            cancel: "Cancel",
            retry: "The previous username was rejected, please try again",
        },
        PromptKind::HttpsPassword { url, username } => PromptTexts {
            icon: AppIcon::Lock,
            title: "Introduce your remote server credentials".into(),
            subtitle: with_operation(match username {
                Some(user) => format!("Password for {user} at {}", without_userinfo(url)),
                None => format!("Password for {}", without_userinfo(url)),
            }),
            hint: Some("Password"),
            masked: true,
            primary: "Continue",
            cancel: "Cancel",
            retry: password_retry,
        },
        PromptKind::SshPassphrase { key_path } => PromptTexts {
            icon: AppIcon::Lock,
            title: "Introduce your SSH key's password".into(),
            subtitle: with_operation(match key_path {
                Some(key) => format!("Your SSH key {key} is protected with a password"),
                None => "Your SSH key is protected with a password".to_owned(),
            }),
            hint: Some("Password"),
            masked: true,
            primary: "Continue",
            cancel: "Cancel",
            retry: password_retry,
        },
        PromptKind::SshPassword { user_host } => PromptTexts {
            icon: AppIcon::Lock,
            title: "Introduce your SSH password".into(),
            subtitle: with_operation(format!("Password for {user_host}")),
            hint: Some("Password"),
            masked: true,
            primary: "Continue",
            cancel: "Cancel",
            retry: password_retry,
        },
        PromptKind::SshPin { device } => PromptTexts {
            icon: AppIcon::Password,
            title: "Introduce your security key's PIN".into(),
            subtitle: with_operation(match device {
                Some(device) => format!("Enter the PIN of {device}"),
                None => "Enter the PIN of your security key".to_owned(),
            }),
            hint: Some("PIN"),
            masked: true,
            primary: "Continue",
            cancel: "Cancel",
            retry: "Invalid PIN, please try again",
        },
        PromptKind::HostKey { host, key_type, .. } => PromptTexts {
            icon: AppIcon::Security,
            title: "Unknown host".into(),
            subtitle: with_operation(match key_type {
                Some(key_type) => format!(
                    "The authenticity of host '{host}' can't be established. It presented a {key_type} key with this fingerprint. Do you want to continue connecting?"
                ),
                None => format!(
                    "The authenticity of host '{host}' can't be established. It presented a key with this fingerprint. Do you want to continue connecting?"
                ),
            }),
            hint: None,
            masked: false,
            primary: "Yes",
            cancel: "No",
            retry: "",
        },
        PromptKind::Other => {
            let raw = request.raw_prompt.trim();
            let lower = raw.to_lowercase();
            let plain = ["username", "user name", "login"]
                .iter()
                .any(|word| lower.contains(word));
            PromptTexts {
                icon: AppIcon::Password,
                title: "Authentication required".into(),
                subtitle: with_operation(raw.to_owned()),
                hint: Some(if plain { "Answer" } else { "Password" }),
                masked: !plain,
                primary: "Continue",
                cancel: "Cancel",
                retry: "The previous answer was rejected, please try again",
            }
        }
    }
}

/// `url` without the `user@` part of its authority (Git's prompt repeats the user name in the URL).
fn without_userinfo(url: &str) -> String {
    let Some(scheme_end) = url.find("://").map(|at| at + 3) else {
        return url.to_owned();
    };
    let authority_end = url[scheme_end..]
        .find('/')
        .map_or(url.len(), |at| scheme_end + at);
    match url[scheme_end..authority_end].rfind('@') {
        Some(at) => format!("{}{}", &url[..scheme_end], &url[scheme_end + at + 1..]),
        None => url.to_owned(),
    }
}

/// Asks for the credential `request` describes; the answer goes to `reply` (dropped = Cancel).
pub fn open(
    window: &mut Window,
    cx: &mut App,
    request: CredentialRequest,
    reply: SyncSender<CredentialResponse>,
) {
    let texts = prompt_texts(&request);
    // A connected forge account of the prompt's host can answer instead of the user (20 §6.3, X4).
    let offer = crate::forge::accounts::https_offer(&request, cx);
    let field = texts.hint.map(|hint| {
        cx.new(|cx| {
            let field = if texts.masked {
                OutlinedField::password(ids::CREDENTIAL_FIELD, window, cx)
            } else {
                OutlinedField::new(ids::CREDENTIAL_FIELD, window, cx)
            };
            let field = field.hint(hint, window, cx);
            if texts.masked {
                // Gitnuro: 8 dp gap, the 20 dp toggle, 4 dp end padding (the field pads 12 itself).
                field.trailing_space(metrics::dialogs::EYE_BUTTON)
            } else {
                field
            }
        })
    });
    let view = cx.new(|cx| {
        let subscription = field.as_ref().map(|field| {
            cx.subscribe_in(
                field,
                window,
                |this: &mut CredentialDialog, _, event: &FieldEvent, window, cx| match event {
                    FieldEvent::Changed => {
                        this.typed = true;
                        cx.notify();
                    }
                    FieldEvent::Enter => this.accept(window, cx),
                },
            )
        });
        CredentialDialog {
            request,
            texts,
            reply: Rc::new(RefCell::new(Some(reply))),
            field,
            offer,
            use_account: false,
            typed: false,
            shown: false,
            focus: FocusOnce::default(),
            layer: None,
            _subscription: subscription,
            _poll: None,
        }
    });
    let poll = view.update(cx, |_, cx| {
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(CANCEL_POLL).await;
                let cancelled = this.update_in(cx, |this, window, cx| {
                    let cancelled = this.request.cancel.is_cancelled();
                    if cancelled {
                        this.answer(CredentialResponse::Cancel, window, cx);
                    }
                    cancelled
                });
                match cancelled {
                    Ok(false) => {}
                    // Cancelled, or the dialog is gone.
                    _ => break,
                }
            }
        })
    });
    view.update(cx, |this, _| this._poll = Some(poll));
    let width = if view.read(cx).field.is_some() {
        CREDENTIAL_W
    } else {
        HOST_KEY_W
    };
    let layer = common::open_view(window, cx, width, 360., view.clone());
    view.update(cx, |this, _| this.layer = Some(layer));
}

/// The content of an open credential dialog.
pub struct CredentialDialog {
    request: CredentialRequest,
    texts: PromptTexts,
    reply: Rc<RefCell<Option<SyncSender<CredentialResponse>>>>,
    field: Option<Entity<OutlinedField>>,
    /// The connected forge account that can answer an HTTPS prompt.
    offer: Option<crate::forge::accounts::AccountOffer>,
    /// "Use {forge} account {user}" is checked: the account answers, the field is ignored.
    use_account: bool,
    /// The user typed since the dialog opened (hides the retry banner).
    typed: bool,
    /// A masked field currently shows its text.
    shown: bool,
    focus: FocusOnce,
    /// The layer of this dialog: it closes by id, so that another dialog opened above it stays open.
    layer: Option<LayerId>,
    _subscription: Option<Subscription>,
    _poll: Option<Task<()>>,
}

impl CredentialDialog {
    /// Sends `response` (once), then closes the dialog.
    fn answer(
        &mut self,
        response: CredentialResponse,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        close_layer_or_top(window, cx, self.layer);
        if let Some(reply) = self.reply.borrow_mut().take() {
            let _ = reply.send(response);
        }
    }

    fn accept(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.use_account
            && let Some(offer) = self.offer.clone()
        {
            self.accept_with_account(&offer, window, cx);
            return;
        }
        let response = match &self.field {
            Some(field) => CredentialResponse::Answer(Sensitive::new(common::text_of(field, cx))),
            None => CredentialResponse::Answer(Sensitive::new("yes".to_owned())),
        };
        self.answer(response, window, cx);
    }

    /// Answers with the connected account: its user name for a user name prompt, its token (read from the keychain
    /// in the background) for a password prompt.
    fn accept_with_account(
        &mut self,
        offer: &crate::forge::accounts::AccountOffer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let is_password = matches!(self.request.kind, PromptKind::HttpsPassword { .. });
        if !is_password {
            let answer = CredentialResponse::Answer(Sensitive::new(offer.username().to_owned()));
            self.answer(answer, window, cx);
            return;
        }
        let token = offer.password(cx);
        let reply = self.reply.borrow_mut().take();
        let label = format!("The {} account {}", offer.forge, offer.user);
        window.close_dialog(cx);
        cx.spawn(async move |_, cx| {
            let response = match token.await {
                Ok(Some(token)) => CredentialResponse::Answer(token),
                // No stored token (or no keychain access): the prompt is declined, Git reports the failure; the
                // toast says why.
                other => {
                    let reason = match other {
                        Err(failure) => failure.message,
                        _ => "no token is stored for it".to_owned(),
                    };
                    cx.update(|cx| {
                        crate::app_view::show_toast(
                            cx,
                            None,
                            crate::notifications::ToastKind::Warning,
                            format!("{label} cannot answer the password prompt: {reason}"),
                            None,
                        );
                    });
                    CredentialResponse::Cancel
                }
            };
            if let Some(reply) = reply {
                let _ = reply.send(response);
            }
        })
        .detach();
    }

    /// What Cancel (or "No") answers.
    fn declined_kind(&self) -> Declined {
        if self.field.is_none() {
            Declined::No
        } else {
            Declined::Cancel
        }
    }
}

/// The answer of the Cancel button: an unknown host key is answered "no", every other prompt is cancelled.
#[derive(Clone, Copy)]
enum Declined {
    No,
    Cancel,
}

impl Declined {
    fn response(self) -> CredentialResponse {
        match self {
            Declined::No => CredentialResponse::Answer(Sensitive::new("no".to_owned())),
            Declined::Cancel => CredentialResponse::Cancel,
        }
    }
}

impl Render for CredentialDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = *cx.tokens();
        if let Some(field) = self.field.clone() {
            self.focus.run(&field, false, window, cx);
        }
        let this = cx.weak_entity();
        let host_key = match &self.request.kind {
            PromptKind::HostKey { fingerprint, .. } => Some(
                fingerprint
                    .clone()
                    .unwrap_or_else(|| self.request.raw_prompt.trim().to_owned()),
            ),
            _ => None,
        };
        let retry = (self.request.attempt > 1 && !self.typed && !self.texts.retry.is_empty())
            .then_some(self.texts.retry);
        let field_block = self.field.clone().map(|field| {
            let eye = self.texts.masked.then(|| {
                let this = this.clone();
                let target = field.clone();
                common::eye_toggle(
                    ids::CREDENTIAL_EYE,
                    self.shown,
                    &tokens,
                    move |window, cx| {
                        this.update(cx, |d, cx| {
                            d.shown = !d.shown;
                            let masked = !d.shown;
                            target.update(cx, |f, cx| {
                                f.set_masked(masked, window, cx);
                                f.focus(window, cx);
                            });
                            cx.notify();
                        })
                        .ok();
                    },
                )
            });
            div()
                .relative()
                .w(dp(CREDENTIAL_W))
                .child(field)
                .when_some(eye, |wrap, eye| {
                    wrap.child(
                        div()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .right(dp(EYE_END_PAD))
                            .flex()
                            .items_center()
                            .child(eye),
                    )
                })
        });
        let width = if self.field.is_some() {
            CREDENTIAL_W
        } else {
            HOST_KEY_W
        };
        let offer_toggle = self.offer.as_ref().map(|offer| {
            let this = this.clone();
            div().w(dp(CREDENTIAL_W)).pt(dp(8.)).child(
                CheckboxText::new(ids::CREDENTIAL_USE_ACCOUNT, self.use_account, offer.label())
                    .on_toggle(move |checked, _, cx| {
                        this.update(cx, |d, cx| {
                            d.use_account = checked;
                            cx.notify();
                        })
                        .ok();
                    }),
            )
        });
        let content = div()
            .w(dp(width))
            .flex()
            .flex_col()
            .items_center()
            .when_some(host_key, |col, fingerprint| {
                col.child(
                    crate::widgets::observed(ids::CREDENTIAL_FINGERPRINT)
                        .w_full()
                        .p(dp(8.))
                        .rounded(dp(metrics::RADIUS))
                        .bg(tokens.secondary_surface)
                        .text_mono13()
                        .child(fingerprint),
                )
            })
            .when_some(field_block, |col, block| col.child(block))
            .when_some(offer_toggle, |col, toggle| col.child(toggle))
            .when_some(retry, |col, text| {
                col.child(
                    crate::widgets::observed(ids::CREDENTIAL_RETRY)
                        .mt(dp(8.))
                        .w_full()
                        .rounded(dp(metrics::RADIUS))
                        .bg(tokens.error)
                        .px(dp(8.))
                        .py(dp(4.))
                        .text_body1()
                        .text_color(tokens.on_primary)
                        .child(text),
                )
            });
        let on_accept = this;
        let on_decline = self.reply.clone();
        let decline = self.declined_kind();
        crate::widgets::observed(ids::CREDENTIAL_DIALOG)
            .w(dp(width))
            .child(
                IconBasedDialog::new(
                    self.texts.icon,
                    self.texts.title.clone(),
                    self.texts.primary,
                )
                .subtitle(self.texts.subtitle.clone())
                .cancel_label(self.texts.cancel)
                .content(content)
                .on_primary(move |window, cx| {
                    on_accept.update(cx, |d, cx| d.accept(window, cx)).ok();
                })
                .on_cancel(move |_, _| {
                    // The button already closed the dialog (and with it this entity): only the answer is left
                    // to send, through the shared reply slot.
                    if let Some(reply) = on_decline.borrow_mut().take() {
                        let _ = reply.send(decline.response());
                    }
                }),
            )
    }
}
