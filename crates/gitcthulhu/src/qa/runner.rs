//! The scenario runner: executes a scenario on the UI thread's executor after the first frame, step by step, and
//! writes the report.
//!
//! No OS input is ever injected: every verb calls the app's own actions and public entry points inside this
//! process. Screenshots are captured from the window's own surface ([`super::capture`]). A watchdog thread ends
//! the process with exit code 124 (after writing a partial report) when the run exceeds its time limit, for
//! example because the UI thread hangs.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gpui_kit::component::WindowExt as _;
use gpui_kit::{App, AsyncWindowContext, Global, Task, Window};
use parking_lot::Mutex;

use super::dsl::{self, Line};
use super::monitor::{self, Monitor};
use super::report::{Report, Status};
use super::verbs::{self, Action, Args, RunnerVerb, Verb};
use super::{QaRun, capture, idle, set_exit_code};

/// How long the app must stay idle before a step counts as settled.
const QUIET: Duration = Duration::from_millis(250);
/// How often idleness is polled.
const POLL: Duration = Duration::from_millis(25);
/// How long the runner waits for the app to settle after a step before it goes on.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(10);
/// Default timeout of `wait idle`.
const WAIT_IDLE_TIMEOUT: Duration = Duration::from_secs(20);
/// Pause between the last frame and the capture, so the compositor has presented it.
const PRESENT_PAUSE: Duration = Duration::from_millis(150);
/// Exit code of a run ended by the watchdog.
pub const EXIT_TIMEOUT: i32 = 124;

/// Set once the run has finished normally (the watchdog stays quiet afterwards).
static FINISHED: AtomicBool = AtomicBool::new(false);

/// The runner's tasks, kept alive for the process.
struct RunnerTasks {
    _tasks: Vec<Task<()>>,
}

impl Global for RunnerTasks {}

/// Starts the scenario of `run` in `window` (after its first frame).
pub fn start(run: &QaRun, window: &mut Window, cx: &mut App) {
    let (Some(scenario), Some(out_dir)) = (run.args.scenario.clone(), run.args.out_dir.clone())
    else {
        return;
    };
    let name = scenario.file_name().map_or_else(
        || scenario.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    let report = Arc::new(Mutex::new(Report {
        scenario: name,
        ..Default::default()
    }));
    if let Err(err) = std::fs::create_dir_all(&out_dir) {
        tracing::error!("cannot create {}: {err}", out_dir.display());
    }
    spawn_watchdog(report.clone(), out_dir.clone(), run.args.timeout);

    let lines = match std::fs::read_to_string(&scenario) {
        Ok(text) => match dsl::parse(&text) {
            Ok(lines) => lines,
            Err(errors) => {
                for e in errors {
                    report
                        .lock()
                        .push(e.number, "parse", Status::Fail, e.message);
                }
                finish(&report, &out_dir, 0, cx);
                return;
            }
        },
        Err(err) => {
            report.lock().push(
                0,
                "read scenario",
                Status::Fail,
                format!("{}: {err}", scenario.display()),
            );
            finish(&report, &out_dir, 0, cx);
            return;
        }
    };

    let sandbox = run.sandbox.root().to_path_buf();
    let sampler = window.spawn(cx, async move |cx| {
        while cx.update(monitor::sample).is_ok() {
            cx.background_executor()
                .timer(Duration::from_millis(20))
                .await;
        }
    });
    let report_for_run = report.clone();
    let out_for_run = out_dir.clone();
    let runner = window.spawn(cx, async move |cx| {
        let quit_code = run_lines(&lines, &sandbox, &out_for_run, &report_for_run, cx).await;
        cx.update(|_, cx| finish(&report_for_run, &out_for_run, quit_code, cx))
            .ok();
    });
    cx.set_global(RunnerTasks {
        _tasks: vec![sampler, runner],
    });
}

/// Writes the report, flushes the settings and ends the application.
fn finish(report: &Arc<Mutex<Report>>, out_dir: &Path, quit_code: i32, cx: &mut App) {
    FINISHED.store(true, Ordering::SeqCst);
    {
        let mut report = report.lock();
        report.notes.extend(
            cx.default_global::<Monitor>()
                .toast_lines()
                .into_iter()
                .map(|t| format!("toast {t}")),
        );
        if let Err(err) = report.write(out_dir) {
            tracing::error!("cannot write the report to {}: {err}", out_dir.display());
        }
        let code = if report.exit_code() != 0 {
            report.exit_code()
        } else {
            quit_code
        };
        set_exit_code(code);
    }
    crate::settings::flush(cx);
    cx.quit();
}

/// Ends the process when the run takes longer than `limit`.
fn spawn_watchdog(report: Arc<Mutex<Report>>, out_dir: PathBuf, limit: Duration) {
    let spawned = std::thread::Builder::new()
        .name("qa-watchdog".into())
        .spawn(move || {
            std::thread::sleep(limit);
            if FINISHED.load(Ordering::SeqCst) {
                return;
            }
            // A UI thread that hangs while it holds the report must not keep the watchdog from ending the run.
            if let Some(mut report) = report.try_lock_for(Duration::from_secs(2)) {
                report.push(
                    0,
                    "watchdog",
                    Status::Fail,
                    format!("the run exceeded {limit:?}; the process was ended"),
                );
                let _ = report.write(&out_dir);
            }
            std::process::exit(EXIT_TIMEOUT);
        });
    if let Err(err) = spawned {
        tracing::error!("cannot start the watchdog: {err}");
    }
}

async fn sleep(cx: &mut AsyncWindowContext, duration: Duration) {
    cx.background_executor().timer(duration).await;
}

/// Runs every step; returns the exit code a `quit` step asked for.
async fn run_lines(
    lines: &[Line],
    sandbox: &Path,
    out_dir: &Path,
    report: &Arc<Mutex<Report>>,
    cx: &mut AsyncWindowContext,
) -> i32 {
    draw_frames(cx, 2).await;
    sleep(cx, PRESENT_PAUSE).await;

    // The verb listing documents what this build understands (regions add verbs).
    if let Ok(listing) = cx.update(|_, cx| verbs::describe(cx))
        && let Err(err) = std::fs::write(out_dir.join("verbs.txt"), listing)
    {
        tracing::warn!("cannot write verbs.txt: {err}");
    }

    // Every verb must exist before anything runs: a typo fails fast instead of after minutes of steps.
    let unknown: Vec<(usize, String)> = cx
        .update(|_, cx| {
            lines
                .iter()
                .filter(|l| verbs::lookup(cx, &l.tokens).is_none())
                .map(|l| (l.number, l.text.clone()))
                .collect()
        })
        .unwrap_or_default();
    if !unknown.is_empty() {
        let mut report = report.lock();
        for (number, text) in unknown {
            report.push(number, &text, Status::Fail, "unknown verb (see verbs.txt)");
        }
        return 0;
    }

    let mut quit_code = 0;
    for line in lines {
        let Some((verb, consumed)) = cx
            .update(|_, cx| verbs::lookup(cx, &line.tokens))
            .ok()
            .flatten()
        else {
            report
                .lock()
                .push(line.number, &line.text, Status::Fail, "unknown verb");
            continue;
        };
        let args = Args::new(line.tokens[consumed..].to_vec(), sandbox.to_path_buf());
        if let Action::Runner(RunnerVerb::Quit) = verb.action {
            quit_code = args.opt(0).and_then(|c| c.parse().ok()).unwrap_or(0);
            report
                .lock()
                .push(line.number, &line.text, Status::Pass, "");
            break;
        }
        let started = Instant::now();
        let (status, detail) = execute(&verb, &args, out_dir, report, cx).await;
        tracing::info!(
            "qa step {}: {} -> {} ({:?})",
            line.number,
            line.text,
            status.as_str(),
            started.elapsed()
        );
        report.lock().push(line.number, &line.text, status, detail);
    }
    quit_code
}

/// Runs one step.
async fn execute(
    verb: &Verb,
    args: &Args,
    out_dir: &Path,
    report: &Arc<Mutex<Report>>,
    cx: &mut AsyncWindowContext,
) -> (Status, String) {
    match &verb.action {
        Action::Sync(handler) => {
            let handler = handler.clone();
            let args = args.clone();
            let result = cx.update(|window, cx| handler(window, cx, &args));
            match result {
                Err(_) => (Status::Fail, "the window is gone".into()),
                Ok(Err(message)) => (Status::Fail, message),
                Ok(Ok(detail)) => {
                    if !verb.settle {
                        return (Status::Pass, detail);
                    }
                    match wait_idle(cx, SETTLE_TIMEOUT).await {
                        Ok(()) => (Status::Pass, detail),
                        Err(busy) => (Status::Pass, format!("{detail} (not settled: {busy})")),
                    }
                }
            }
        }
        Action::Runner(RunnerVerb::WaitIdle) => {
            let timeout = args
                .opt(0)
                .and_then(|ms| ms.parse::<u64>().ok())
                .map_or(WAIT_IDLE_TIMEOUT, Duration::from_millis);
            match wait_idle(cx, timeout).await {
                Ok(()) => (Status::Pass, String::new()),
                Err(busy) => (Status::Fail, busy),
            }
        }
        Action::Runner(RunnerVerb::WaitDialog) => {
            let want_open = match args.text(0, "open or closed") {
                Ok(word) => match word.to_lowercase().as_str() {
                    "open" => true,
                    "closed" => false,
                    other => return (Status::Fail, format!("'{other}' is not open or closed")),
                },
                Err(message) => return (Status::Fail, message),
            };
            let timeout = args
                .opt(1)
                .and_then(|ms| ms.parse::<u64>().ok())
                .map_or(WAIT_IDLE_TIMEOUT, Duration::from_millis);
            match wait_dialog(cx, want_open, timeout).await {
                Ok(()) => (Status::Pass, String::new()),
                Err(message) => (Status::Fail, message),
            }
        }
        Action::Runner(RunnerVerb::Wait) => match args.number::<u64>(0, "milliseconds") {
            Ok(ms) => {
                sleep(cx, Duration::from_millis(ms)).await;
                (Status::Pass, String::new())
            }
            Err(message) => (Status::Fail, message),
        },
        Action::Runner(RunnerVerb::Shot) => shot(args, out_dir, report, cx).await,
        Action::Runner(RunnerVerb::Quit) => (Status::Pass, String::new()),
    }
}

/// Waits until a dialog is open (or closed), then until two frames were drawn.
async fn wait_dialog(
    cx: &mut AsyncWindowContext,
    want_open: bool,
    timeout: Duration,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        let open = cx
            .update(|window, cx| window.has_active_dialog(cx))
            .map_err(|_| "the window is gone".to_owned())?;
        if open == want_open {
            break;
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "no dialog {} after {timeout:?}",
                if want_open { "opened" } else { "closed" }
            ));
        }
        sleep(cx, POLL).await;
    }
    if !want_open {
        // The prompts `begin action` expected are over.
        let _ = cx.update(|_, cx| monitor::expect_prompts(cx, false));
    }
    draw_frames(cx, 2).await;
    Ok(())
}

/// Waits until the app is idle (no probe reports work) for [`QUIET`], then until two frames were drawn.
async fn wait_idle(cx: &mut AsyncWindowContext, timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    let mut quiet_since: Option<Instant> = None;
    let mut last_reason = String::new();
    loop {
        let busy = cx
            .update(idle::busy_reason)
            .map_err(|_| "the window is gone".to_owned())?;
        match busy {
            None => {
                let since = *quiet_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= QUIET {
                    break;
                }
            }
            Some(reason) => {
                quiet_since = None;
                last_reason = reason;
            }
        }
        if Instant::now() >= deadline {
            return Err(format!("still busy after {timeout:?}: {last_reason}"));
        }
        sleep(cx, POLL).await;
    }
    draw_frames(cx, 2).await;
    Ok(())
}

/// Requests `count` frames and waits until they were drawn (gives up after a second per frame, for example when
/// the window is hidden).
async fn draw_frames(cx: &mut AsyncWindowContext, count: usize) {
    for _ in 0..count {
        let (tx, rx) = futures::channel::oneshot::channel::<()>();
        let requested = cx.update(|window, _| {
            window.on_next_frame(move |_, _| {
                let _ = tx.send(());
            });
            window.refresh();
        });
        if requested.is_err() {
            return;
        }
        let timeout = cx.background_executor().timer(Duration::from_secs(1));
        futures::future::select(rx, timeout).await;
    }
}

/// `shot <name> [window]`.
async fn shot(
    args: &Args,
    out_dir: &Path,
    report: &Arc<Mutex<Report>>,
    cx: &mut AsyncWindowContext,
) -> (Status, String) {
    let name = match args.text(0, "screenshot name") {
        Ok(name) => name,
        Err(message) => return (Status::Fail, message),
    };
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return (
            Status::Fail,
            format!("'{name}' is not a file name (letters, digits, - _ . only)"),
        );
    }
    let whole_window = args.has_flag(1, "window");
    if let Err(busy) = wait_idle(cx, SETTLE_TIMEOUT).await {
        tracing::warn!("shot {name}: {busy}");
    }
    sleep(cx, PRESENT_PAUSE).await;
    let Ok((hwnd, scale)) =
        cx.update(|window, _| (capture::window_hwnd(window), window.scale_factor()))
    else {
        return (Status::Fail, "the window is gone".into());
    };
    let Some(hwnd) = hwnd else {
        return (
            Status::Skip,
            "no native window handle (screenshots need Windows)".into(),
        );
    };
    let path = out_dir.join(format!("{name}.png"));
    let target = path.clone();
    // Off the UI thread: PrintWindow sends a message to the window, which this thread must be free to handle.
    let captured = cx
        .background_executor()
        .spawn(async move {
            let frame = capture::capture(hwnd, whole_window)?;
            if capture::is_uniform(&frame) {
                return Err(format!(
                    "the capture is a single colour ({}x{}): the window was not composited",
                    frame.width, frame.height
                ));
            }
            capture::write_png(&target, &frame)?;
            Ok((frame.width, frame.height))
        })
        .await;
    match captured {
        Ok((width, height)) => {
            report.lock().shots.push(path.clone());
            (
                Status::Pass,
                format!("{} ({width}x{height} px, scale {scale})", path.display()),
            )
        }
        Err(message) => (Status::Fail, message),
    }
}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;

    use super::*;

    #[test]
    fn the_watchdog_exit_code_is_distinct() {
        assert_eq!(EXIT_TIMEOUT, 124, "like coreutils timeout");
        assert!(QUIET > POLL, "a quiet period spans several polls");
    }
}
