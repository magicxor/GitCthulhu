//! Watches the window for problems while a scenario runs: error toasts (which disappear after a few seconds, so
//! they are sampled every few milliseconds and remembered) and unexpected dialogs.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use gpui_kit::component::WindowExt as _;
use gpui_kit::{App, Global, Window};

use crate::app_view::{AppViewGlobal, ToastHostGlobal};
use crate::notifications::ToastKind;

/// What the sampler has seen so far (an app global).
#[derive(Default)]
pub struct Monitor {
    /// Ids of toasts already recorded.
    seen: HashSet<u64>,
    /// Every toast shown, in order: kind label and text.
    toasts: Vec<(ToastKind, String)>,
    /// Descriptions of unexpected dialogs.
    dialogs: Vec<String>,
    /// When the scenario last opened a dialog itself (such a dialog is not unexpected).
    scenario_dialog: Option<Instant>,
    /// The scenario opened an error dialog itself (`open dialog error`).
    scenario_error: bool,
}

impl Global for Monitor {}

impl Monitor {
    /// Texts of the error toasts seen so far.
    pub fn error_toasts(&self) -> Vec<String> {
        self.toasts
            .iter()
            .filter(|(kind, _)| *kind == ToastKind::Error)
            .map(|(_, text)| text.clone())
            .collect()
    }

    /// Whether a toast with `text` inside was shown (any kind).
    pub fn saw_toast(&self, text: &str) -> bool {
        self.toasts.iter().any(|(_, t)| t.contains(text))
    }

    /// Descriptions of the unexpected dialogs seen so far.
    pub fn unexpected_dialogs(&self) -> &[String] {
        &self.dialogs
    }

    /// Every toast seen, `kind: text`, for the report.
    pub fn toast_lines(&self) -> Vec<String> {
        self.toasts
            .iter()
            .map(|(kind, text)| format!("{kind:?}: {text}"))
            .collect()
    }
}

/// Tells the monitor that the scenario opened (`true`) or closed (`false`) a dialog itself, so it is not reported.
pub fn scenario_dialog(cx: &mut App, open: bool) {
    cx.default_global::<Monitor>().scenario_dialog = open.then(Instant::now);
}

/// Tells the monitor that the scenario queued an error dialog itself (`open dialog error`).
pub fn scenario_error(cx: &mut App) {
    let monitor = cx.default_global::<Monitor>();
    monitor.scenario_dialog = Some(Instant::now());
    monitor.scenario_error = true;
}

/// A dialog takes a frame or two to appear: the scenario's flag is kept at least this long.
const DIALOG_GRACE: Duration = Duration::from_millis(1500);

/// One sample: records new toasts, queued error dialogs and dialogs the scenario did not open.
pub fn sample(window: &mut Window, cx: &mut App) {
    let mut fresh: Vec<(u64, ToastKind, String)> = Vec::new();
    if let Some(host) = cx.try_global::<ToastHostGlobal>() {
        let seen = cx.try_global::<Monitor>().map(|m| &m.seen);
        // Every tab's toasts: an operation of a background tab can fail while another tab is shown.
        for toast in host.0.read(cx).all() {
            if !seen.is_some_and(|seen| seen.contains(&toast.id)) {
                fresh.push((toast.id, toast.kind, toast.text.to_string()));
            }
        }
    }
    let (queued, error_open) = cx
        .try_global::<AppViewGlobal>()
        .and_then(|g| g.0.upgrade())
        .map_or((0, false), |view| {
            let view = view.read(cx);
            (view.pending_errors(), view.error_dialog_open())
        });
    let dialog_open = window.has_active_dialog(cx);
    let monitor = cx.default_global::<Monitor>();
    for (id, kind, text) in fresh {
        monitor.seen.insert(id);
        monitor.toasts.push((kind, text));
    }
    if queued > 0 {
        note_dialog(monitor, format!("{queued} error dialog(s) queued"));
    }
    // An error dialog counts even while a dialog of the scenario's own is flagged (an action inside it failed).
    if error_open && !monitor.scenario_error {
        note_dialog(monitor, "an error dialog is showing".into());
    }
    if !dialog_open {
        // Closed (Escape, a button): the next dialog is not the scenario's unless it opens it.
        if monitor
            .scenario_dialog
            .is_some_and(|at| at.elapsed() > DIALOG_GRACE)
        {
            monitor.scenario_dialog = None;
            monitor.scenario_error = false;
        }
    } else if monitor.scenario_dialog.is_none() {
        note_dialog(
            monitor,
            "a dialog is open that the scenario did not open".into(),
        );
    }
}

fn note_dialog(monitor: &mut Monitor, text: String) {
    if !monitor.dialogs.contains(&text) {
        monitor.dialogs.push(text);
    }
}
