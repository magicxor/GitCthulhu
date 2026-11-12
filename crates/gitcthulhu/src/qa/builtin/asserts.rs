//! Assertion verbs: they look at the state as it is (the runner does not wait after them) and fail the step when
//! the expectation does not hold.

use gpui_kit::App;
use gpui_kit::component::WindowExt as _;

use super::{active_ctx, app_view};
use crate::qa::monitor::{self, Monitor};
use crate::qa::verbs::{Verb, register_verb};

/// Registers the verbs of this file.
pub(super) fn register(cx: &mut App) {
    register_verb(
        cx,
        Verb::new(
            "assert no error toast",
            "assert no error toast",
            "Fails if an error toast was shown at any time since the run started",
            |window, cx, _| {
                monitor::sample(window, cx);
                let errors = cx.default_global::<Monitor>().error_toasts();
                if errors.is_empty() {
                    Ok(String::new())
                } else {
                    Err(format!("error toast(s): {}", errors.join(" | ")))
                }
            },
        )
        .without_settle(),
    );
    register_verb(
        cx,
        Verb::new(
            "assert no error dialog",
            "assert no error dialog",
            "Fails if an error dialog was queued or an unexpected dialog was open at any time since the run started",
            |window, cx, _| {
                monitor::sample(window, cx);
                let dialogs = cx.default_global::<Monitor>().unexpected_dialogs().to_vec();
                if dialogs.is_empty() {
                    Ok(String::new())
                } else {
                    Err(dialogs.join(" | "))
                }
            },
        )
        .without_settle(),
    );
    register_verb(
        cx,
        Verb::new(
            "assert no error",
            "assert no error",
            "assert no error toast + assert no error dialog",
            |window, cx, _| {
                monitor::sample(window, cx);
                let monitor = cx.default_global::<Monitor>();
                let mut problems = monitor.error_toasts();
                problems.extend(monitor.unexpected_dialogs().iter().cloned());
                if problems.is_empty() {
                    Ok(String::new())
                } else {
                    Err(problems.join(" | "))
                }
            },
        )
        .without_settle(),
    );
    register_verb(
        cx,
        Verb::new(
            "assert toast",
            "assert toast <text>",
            "Fails unless a toast containing the text was shown since the run started",
            |window, cx, args| {
                monitor::sample(window, cx);
                let text = args.rest(0);
                if cx.default_global::<Monitor>().saw_toast(&text) {
                    Ok(String::new())
                } else {
                    let seen = cx.default_global::<Monitor>().toast_lines();
                    Err(format!(
                        "no toast contains '{text}' (seen: {})",
                        seen.join(" | ")
                    ))
                }
            },
        )
        .without_settle(),
    );
    register_verb(
        cx,
        Verb::new(
            "assert tab count",
            "assert tab count <N>",
            "Fails unless exactly N tabs are open",
            |_, cx, args| {
                let expected: usize = args.number(0, "tab count")?;
                let actual = app_view(cx)?.read(cx).tabs().len();
                if actual == expected {
                    Ok(String::new())
                } else {
                    Err(format!("{actual} tabs are open, expected {expected}"))
                }
            },
        )
        .without_settle(),
    );
    register_verb(
        cx,
        Verb::new(
            "assert tab title",
            "assert tab title <text>",
            "Fails unless the selected tab's title contains the text",
            |_, cx, args| {
                let text = args.rest(0);
                let view = app_view(cx)?;
                let view = view.read(cx);
                let title = view.tab_title(view.active(), cx).to_string();
                if title.to_lowercase().contains(&text.to_lowercase()) {
                    Ok(title)
                } else {
                    Err(format!(
                        "the selected tab is titled '{title}', expected '{text}'"
                    ))
                }
            },
        )
        .without_settle(),
    );
    register_verb(
        cx,
        Verb::new(
            "assert dialog",
            "assert dialog open|closed",
            "Fails unless a dialog is (not) open",
            |window, cx, args| {
                let want_open = match args.text(0, "open or closed")?.to_lowercase().as_str() {
                    "open" => true,
                    "closed" => false,
                    other => return Err(format!("'{other}' is not open or closed")),
                };
                let is_open = window.has_active_dialog(cx);
                if is_open == want_open {
                    Ok(String::new())
                } else {
                    Err(format!(
                        "a dialog is {}",
                        if is_open { "open" } else { "closed" }
                    ))
                }
            },
        )
        .without_settle(),
    );
    register_verb(
        cx,
        Verb::new(
            "assert log rows",
            "assert log rows <N>",
            "Fails unless the log shows N rows (the uncommitted row counts while it is shown)",
            |_, cx, args| {
                let expected: usize = args.number(0, "row count")?;
                let ctx = active_ctx(cx)?;
                let model = ctx.model.read(cx);
                let loaded = model
                    .log
                    .value
                    .as_ref()
                    .ok_or_else(|| "the log is not loaded".to_owned())?
                    .rows
                    .len();
                let actual = loaded.saturating_sub(usize::from(!model.show_uncommitted_row()));
                if actual == expected {
                    Ok(String::new())
                } else {
                    Err(format!("the log shows {actual} rows, expected {expected}"))
                }
            },
        )
        .without_settle(),
    );
}
