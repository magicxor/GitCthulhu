//! Credential prompts per `PromptKind` (owned by G10 after G0b; 09 §3.15, §3.16, spec 21 §2.8, 20 §6.2). The
//! milestone-1 placeholder declines the prompt (the reply is dropped, so Git sees Cancel) and says so in a toast;
//! G10 implements the dialogs, the retry banner, the 200 ms cancel-token polling and `forge::accounts::https_offer`.

use std::sync::mpsc::SyncSender;

use gpui_kit::{App, Window};

use crate::backend::{CredentialRequest, CredentialResponse};

/// Asks for the credential `request` describes; the answer goes to `reply` (dropped = Cancel).
pub fn open(
    _window: &mut Window,
    cx: &mut App,
    request: CredentialRequest,
    reply: SyncSender<CredentialResponse>,
) {
    let _ = reply.send(CredentialResponse::Cancel);
    crate::app_view::not_implemented(
        cx,
        None,
        &format!("credential prompts ({})", request.operation),
    );
}
