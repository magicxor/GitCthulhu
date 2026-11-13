//! The QA scenario runner (feature `qa`; never part of a release build).
//!
//! `tools/qa/run-gui.ps1` starts the app with `--qa-scenario <file> --qa-out <dir>` against a sandbox made by
//! `tools/qa/new-sandbox.ps1`. The app then
//!
//! 1. runs the sandbox gate ([`gate`]) and refuses to start (exit code 3) unless every data location points into
//!    the sandbox, Git is isolated and the network is off,
//! 2. opens its main window (1280x720 logical pixels, not focused, no services, no update check),
//! 3. runs the scenario in-process ([`runner`]): each line is a verb of the verb table ([`verbs`], built-ins in
//!    [`builtin`]) that calls the app's own actions and entry points on the UI thread, then waits until the app is
//!    idle ([`idle`]); `shot <name>` captures the window's own surface into a PNG ([`capture`]),
//! 4. writes `report.txt`, `report.json` and `verbs.txt` into the output directory and exits with 0 (all steps
//!    passed), 1 (a step failed), 2 (usage error), 3 (gate refused) or 124 (watchdog).
//!
//! No OS-level input is ever injected, no other window is touched and nothing outside the sandbox is read.
//!
//! # Extending
//!
//! Regions add scenario verbs with [`register_verb`] and idle conditions with [`register_idle_probe`] from their
//! own `register(cx)` (see [`verbs`] for the exact shape), guarded by `#[cfg(feature = "qa")]`.
//!
//! # Hooks in the app
//!
//! `app::run` calls [`enter`] first, [`init`] before the actions are registered and [`attach`] once the main
//! window exists; it also uses [`window_bounds`] and skips the services and the window activation in QA mode.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicI32, Ordering};

use gpui_kit::{AnyWindowHandle, App, Bounds, Global, WindowBounds, point, px, size};

pub mod args;
pub mod builtin;
pub mod capture;
pub mod dsl;
pub mod gate;
pub mod idle;
pub mod monitor;
pub mod report;
pub mod runner;
pub mod verbs;

pub use idle::{IdleProbe, register_idle_probe};
pub use verbs::{Args, StepResult, Verb, register_verb};

/// Exit code of a command-line usage error.
pub const EXIT_USAGE: i32 = 2;

/// The validated start of a qa run.
#[derive(Clone, Debug)]
pub struct QaRun {
    /// The sandbox the gate accepted.
    pub sandbox: gate::Sandbox,
    /// The `--qa-*` flags.
    pub args: args::QaArgs,
}

/// The run's session (an app global): what verbs may need to know.
pub struct Session {
    /// The sandbox.
    pub sandbox: gate::Sandbox,
    /// The output directory of a scenario run.
    pub out_dir: Option<PathBuf>,
}

impl Global for Session {}

/// The session of the run, if installed.
pub fn session(cx: &App) -> Option<&Session> {
    cx.try_global::<Session>()
}

static ACTIVE: OnceLock<()> = OnceLock::new();
static EXIT_CODE: AtomicI32 = AtomicI32::new(0);

/// Whether this process passed the gate (QA mode: no services, no network, sandboxed data).
pub fn active() -> bool {
    ACTIVE.get().is_some()
}

/// The exit code `main` returns after the application loop ended.
pub fn exit_code() -> i32 {
    EXIT_CODE.load(Ordering::SeqCst)
}

pub(crate) fn set_exit_code(code: i32) {
    EXIT_CODE.store(code, Ordering::SeqCst);
}

/// Runs the sandbox gate and parses the flags; ends the process (exit code 3 or 2) when either refuses. Call
/// before anything is read.
pub fn enter() -> QaRun {
    let sandbox = match gate::check_process_env() {
        Ok(sandbox) => sandbox,
        Err(reason) => {
            eprintln!("Refusing to run: {reason}");
            std::process::exit(gate::EXIT_REFUSED);
        }
    };
    let args = match args::parse(std::env::args_os().skip(1)) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("error: {message}");
            std::process::exit(EXIT_USAGE);
        }
    };
    let _ = ACTIVE.set(());
    install_panic_hook(args.out_dir.clone());
    QaRun { sandbox, args }
}

/// A panic of a qa run leaves `panic.txt` in the output directory (next to the missing report).
fn install_panic_hook(out_dir: Option<PathBuf>) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Some(dir) = &out_dir {
            let _ = std::fs::create_dir_all(dir);
            let _ = std::fs::write(dir.join("panic.txt"), format!("{info}\n"));
        }
        previous(info);
    }));
}

/// Installs the session and the built-in verbs; call before the actions (and with them the regions) register.
pub fn init(cx: &mut App, run: &QaRun) {
    cx.set_global(Session {
        sandbox: run.sandbox.clone(),
        out_dir: run.args.out_dir.clone(),
    });
    builtin::register_all(cx);
}

/// The main window's bounds in QA mode: a floating 1280x720 window (the layout specs' reference size).
pub fn window_bounds() -> WindowBounds {
    WindowBounds::Windowed(Bounds {
        origin: point(px(40.0), px(40.0)),
        size: size(px(1280.0), px(720.0)),
    })
}

/// Starts the scenario once the main window exists ([`enter`] guarantees there is one).
pub fn attach(run: &QaRun, window: AnyWindowHandle, cx: &mut App) {
    let run = run.clone();
    if let Err(err) = window.update(cx, move |_, window, cx| runner::start(&run, window, cx)) {
        tracing::error!("cannot start the QA runner: {err}");
        set_exit_code(1);
        cx.quit();
    }
}

#[cfg(test)]
mod tests;
