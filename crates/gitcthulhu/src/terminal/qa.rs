//! QA scenario verbs of the embedded terminal (feature `qa`; `tools/qa/scenarios/m2-terminal.qa`), and an idle
//! probe so `wait idle` waits for a starting shell's first frame.

use std::time::{Duration, Instant};

use gpui_kit::{App, Entity, Global, WeakEntity, Window};

use super::{TerminalPanel, TerminalView, panel_of};
use crate::app_view::AppViewGlobal;
use crate::qa::{Args, StepResult, Verb, register_idle_probe, register_verb};
use crate::settings::TerminalShell;
use crate::tab::TabCtx;

/// Registers the verbs and the idle probe.
pub(super) fn register(cx: &mut App) {
    register_verb(
        cx,
        Verb::new(
            "show terminal",
            "show terminal",
            "Shows the terminal panel of the selected repository (opens a terminal the first time)",
            |window, cx, _| {
                let panel = panel(cx)?;
                panel.update(cx, |panel, cx| panel.show(window, cx));
                Ok(String::new())
            },
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "hide terminal",
            "hide terminal",
            "Hides the terminal panel of the selected repository (its terminals keep running)",
            |window, cx, _| {
                let panel = panel(cx)?;
                panel.update(cx, |panel, cx| panel.hide(window, cx));
                Ok(String::new())
            },
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "toggle terminal",
            "toggle terminal",
            "Toggles the terminal panel like the toolbar button and Ctrl+`",
            |window, cx, _| {
                let ctx = ctx(cx)?;
                super::toggle(&ctx, window, cx);
                Ok(String::new())
            },
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "new terminal",
            "new terminal [auto|pwsh|powershell|cmd|bash|wsl]",
            "Opens another terminal in the panel, with the default or the named shell",
            new_terminal,
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "select terminal",
            "select terminal <number from 1>",
            "Selects a terminal tab of the panel",
            |window, cx, args| {
                let number: usize = args.number(0, "terminal number")?;
                let panel = panel(cx)?;
                let count = panel.read(cx).terminals().count();
                if number == 0 || number > count {
                    return Err(format!("there are {count} terminals"));
                }
                panel.update(cx, |panel, cx| panel.select(number - 1, window, cx));
                Ok(String::new())
            },
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "terminal input",
            "terminal input <text>",
            "Types <text> and Enter into the selected terminal",
            |window, cx, args| {
                let text = format!("{}\r", args.rest(0));
                let view = active_terminal(cx)?;
                view.update(cx, |view, cx| view.send_text(&text, window, cx));
                Ok(String::new())
            },
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "resize terminal",
            "resize terminal <height in dp>",
            "Sets the panel's height (header and terminal) as a drag of its top edge would, clamped to the window",
            |window, cx, args| {
                let height: f32 = args.number(0, "height")?;
                let panel = panel(cx)?;
                panel.update(cx, |panel, cx| panel.set_height(height, window, cx));
                Ok(String::new())
            },
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "wait terminal shows",
            "wait terminal shows <text>",
            "Waits (up to 10 s) until the selected terminal's screen contains <text>, as a command's output arrives",
            wait_shows,
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "assert terminal shows",
            "assert terminal shows <text>",
            "Fails unless the selected terminal's screen contains <text>",
            assert_shows,
        )
        .without_settle(),
    );
    register_idle_probe(cx, idle_probe);
}

fn new_terminal(window: &mut Window, cx: &mut App, args: &Args) -> StepResult {
    let choice = match args
        .opt(0)
        .as_deref()
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        None | Some("auto") => None,
        Some("pwsh") => Some(TerminalShell::Pwsh),
        Some("powershell") => Some(TerminalShell::WindowsPowerShell),
        Some("cmd") => Some(TerminalShell::Cmd),
        Some("bash") => Some(TerminalShell::GitBash),
        Some("wsl") => Some(TerminalShell::Wsl),
        Some(other) => return Err(format!("unknown shell '{other}'")),
    };
    let panel = panel(cx)?;
    panel.update(cx, |panel, cx| {
        panel.show(window, cx);
        panel.open_terminal(choice, window, cx);
    });
    Ok(String::new())
}

fn assert_shows(_: &mut Window, cx: &mut App, args: &Args) -> StepResult {
    let text = args.rest(0);
    let view = active_terminal(cx)?;
    let screen = view.read(cx).screen_text();
    if screen.contains(&text) {
        Ok(format!("found '{text}'"))
    } else {
        Err(format!(
            "'{text}' is not on the terminal's screen:\n{screen}"
        ))
    }
}

/// How long `wait terminal shows` keeps the app busy at most (the runner's settle timeout).
const EXPECT_FOR: Duration = Duration::from_secs(10);

/// What `wait terminal shows` waits for: text on a terminal's screen, until a deadline.
struct Expectation {
    view: WeakEntity<TerminalView>,
    text: String,
    until: Instant,
}

#[derive(Default)]
struct Expected(Option<Expectation>);

impl Global for Expected {}

/// Sets the expectation the idle probe reports as work until the text shows; the runner then settles on it.
fn wait_shows(_: &mut Window, cx: &mut App, args: &Args) -> StepResult {
    let text = args.rest(0);
    if text.is_empty() {
        return Err("usage: wait terminal shows <text>".into());
    }
    let view = active_terminal(cx)?;
    cx.default_global::<Expected>().0 = Some(Expectation {
        view: view.downgrade(),
        text: text.clone(),
        until: Instant::now() + EXPECT_FOR,
    });
    Ok(format!("waiting for '{text}'"))
}

/// Busy while the selected repository's visible panel starts a shell, while its terminal has not printed the
/// shell's banner or prompt yet, and while an unexpired `wait terminal shows` is not met.
fn idle_probe(_: &mut Window, cx: &App) -> Option<String> {
    if let Some(expected) = cx.try_global::<Expected>().and_then(|e| e.0.as_ref())
        && Instant::now() < expected.until
        && let Some(view) = expected.view.upgrade()
        && !view.read(cx).screen_text().contains(&expected.text)
    {
        return Some(format!(
            "the terminal does not show '{}' yet",
            expected.text
        ));
    }
    let ctx = ctx(cx).ok()?;
    let panel = panel_of(&ctx, cx)?;
    let panel = panel.read(cx);
    if !panel.is_visible() {
        return None;
    }
    if panel.is_starting() {
        return Some("a terminal is starting".into());
    }
    panel
        .active_terminal()
        .filter(|view| {
            let view = view.read(cx);
            !view.has_printed() && !view.has_exited()
        })
        .map(|_| "the shell has not printed its prompt yet".into())
}

fn ctx(cx: &App) -> Result<TabCtx, String> {
    let view = cx
        .try_global::<AppViewGlobal>()
        .and_then(|global| global.0.upgrade())
        .ok_or_else(|| "the window content is gone".to_owned())?;
    let view = view.read(cx);
    let tab = view
        .active_tab()
        .ok_or_else(|| "the selected tab is not a repository (open one first)".to_owned())?;
    Ok(tab.read(cx).ctx().clone())
}

fn panel(cx: &App) -> Result<Entity<TerminalPanel>, String> {
    let ctx = ctx(cx)?;
    panel_of(&ctx, cx).ok_or_else(|| "the selected repository has no terminal panel".to_owned())
}

fn active_terminal(cx: &App) -> Result<Entity<TerminalView>, String> {
    panel(cx)?
        .read(cx)
        .active_terminal()
        .cloned()
        .ok_or_else(|| "the panel has no terminal yet (show terminal, then wait idle)".to_owned())
}
