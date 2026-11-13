//! Questions from background threads to the UI: credential prompts and yes/no confirmations (spec 21 §2.4, §2.8),
//! and the progress pump of a running operation.

use std::sync::Arc;
use std::sync::mpsc::{RecvTimeoutError, SyncSender, sync_channel};
use std::time::Duration;

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use gpui_kit::SharedString;

use super::{
    CancelToken, CredentialPrompter, CredentialRequest, CredentialResponse, OpContext, Progress,
    ProgressSink,
};
use crate::tab::TabId;

/// How often a blocked background thread checks whether its question is still wanted.
const POLL: Duration = Duration::from_millis(100);

/// A question a background thread asks the UI; AppView's request pump answers it (spec 21 §2.8).
pub enum UiRequest {
    /// Git asks for a credential. `reply` dropped or `Cancel` = declined.
    Credential {
        tab: Option<TabId>,
        request: CredentialRequest,
        reply: SyncSender<CredentialResponse>,
    },
    /// A yes/no question. `reply` dropped = no.
    Confirm {
        tab: Option<TabId>,
        request: ConfirmRequest,
        reply: SyncSender<bool>,
    },
}

impl UiRequest {
    /// The tab that asks, if any.
    pub fn tab(&self) -> Option<TabId> {
        match self {
            UiRequest::Credential { tab, .. } | UiRequest::Confirm { tab, .. } => *tab,
        }
    }
}

/// Text of a confirmation dialog.
#[derive(Clone, Debug, PartialEq)]
pub struct ConfirmRequest {
    pub title: SharedString,
    pub message: SharedString,
    /// Extra lines (paths, refs) listed under the message.
    pub details: Vec<SharedString>,
    pub confirm_label: SharedString,
    pub cancel_label: SharedString,
    /// The confirm button uses the error colour.
    pub destructive: bool,
}

impl ConfirmRequest {
    /// A question with "Continue" / "Cancel" buttons.
    pub fn new(title: impl Into<SharedString>, message: impl Into<SharedString>) -> Self {
        Self {
            title: title.into(),
            message: message.into(),
            details: Vec::new(),
            confirm_label: "Continue".into(),
            cancel_label: "Cancel".into(),
            destructive: false,
        }
    }
}

/// Sends [`UiRequest`]s to the window's request pump.
#[derive(Clone)]
pub struct UiRequestSender(pub(crate) UnboundedSender<UiRequest>);

impl UiRequestSender {
    /// A sender and the receiver AppView pumps.
    pub fn channel() -> (UiRequestSender, UnboundedReceiver<UiRequest>) {
        let (tx, rx) = unbounded();
        (UiRequestSender(tx), rx)
    }

    /// Queues `request`; `false` when the pump is gone (the window closed).
    pub fn send(&self, request: UiRequest) -> bool {
        self.0.unbounded_send(request).is_ok()
    }
}

/// `CredentialPrompter` over the request pump. Called on the executor's blocking threads (never the UI thread):
/// sends [`UiRequest::Credential`] and waits for the answer, returning `Cancel` when the request's token fires, the
/// pump is gone or the dialog dropped its reply.
pub struct GuiPrompter {
    pub tx: UiRequestSender,
    pub tab: Option<TabId>,
}

impl CredentialPrompter for GuiPrompter {
    fn prompt(&self, request: &CredentialRequest) -> CredentialResponse {
        let (reply, answer) = sync_channel(1);
        if !self.tx.send(UiRequest::Credential {
            tab: self.tab,
            request: request.clone(),
            reply,
        }) {
            return CredentialResponse::Cancel;
        }
        wait(&answer, &request.cancel).unwrap_or(CredentialResponse::Cancel)
    }
}

fn wait<T>(answer: &std::sync::mpsc::Receiver<T>, cancel: &CancelToken) -> Option<T> {
    loop {
        if cancel.is_cancelled() {
            return None;
        }
        match answer.recv_timeout(POLL) {
            Ok(value) => return Some(value),
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return None,
        }
    }
}

/// Blocking yes/no questions from a background thread (never while holding the repository guard).
#[derive(Clone)]
pub enum Asker {
    /// Asks through the request pump.
    Ui {
        tx: UiRequestSender,
        tab: Option<TabId>,
    },
    /// Tests: answers immediately.
    Auto(bool),
}

impl Asker {
    /// Asks `request` and blocks until answered; `false` when the pump or the dialog went away.
    pub fn confirm(&self, request: ConfirmRequest) -> bool {
        match self {
            Asker::Auto(answer) => *answer,
            Asker::Ui { tx, tab } => {
                let (reply, answer) = sync_channel(1);
                if !tx.send(UiRequest::Confirm {
                    tab: *tab,
                    request,
                    reply,
                }) {
                    return false;
                }
                // Never cancelled from outside: the dialog always answers or drops the reply.
                wait(&answer, &CancelToken::new()).unwrap_or(false)
            }
        }
    }
}

/// `ProgressSink` into an unbounded channel; never blocks (20 §1.6). A UI task drains it.
pub struct ProgressPump(pub UnboundedSender<Progress>);

impl ProgressPump {
    /// A pump and the receiver the UI drains.
    pub fn channel() -> (ProgressPump, UnboundedReceiver<Progress>) {
        let (tx, rx) = unbounded();
        (ProgressPump(tx), rx)
    }
}

impl ProgressSink for ProgressPump {
    fn progress(&self, e: Progress) {
        let _ = self.0.unbounded_send(e);
    }
}

/// The owned `OpContext` of a tab operation.
pub fn op_context(
    cancel: CancelToken,
    progress: ProgressPump,
    prompter: Arc<dyn CredentialPrompter>,
    label: String,
) -> OpContext {
    OpContext {
        cancel,
        progress: Arc::new(progress),
        prompter,
        label,
    }
}

/// The progress line of the processing overlay for one event: `"{phase} {percent}%"`,
/// `"{phase} {current}/{total}"`, `"{phase} {current}"` or a message; `None` for events that are not shown.
pub fn progress_text(event: &Progress) -> Option<SharedString> {
    match event {
        Progress::Phase {
            phase,
            current,
            total,
            percent,
            ..
        } => Some(SharedString::from(match (percent, total) {
            (Some(p), _) => format!("{phase} {p}%"),
            (None, Some(t)) => format!("{phase} {current}/{t}"),
            (None, None) => format!("{phase} {current}"),
        })),
        Progress::Message(text) if !text.trim().is_empty() => {
            Some(SharedString::from(text.trim().to_owned()))
        }
        _ => None,
    }
}
