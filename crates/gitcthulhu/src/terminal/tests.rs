use core::prelude::v1::test;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use but_tty::{RgbColor, Row, Run, ShellKind, Style, Underline};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    AnyWindowHandle, AppContext as _, Bounds, Entity, Focusable as _, InteractiveElement as _,
    Keystroke, TestAppContext, div, point, px, size,
};

use super::grid::{self, Grid};
use super::keys::{self, KeyRoute};
use super::shell::{self, Probe};
use super::*;
use crate::app_view::AppView;
use crate::metrics;
use crate::settings::TerminalShell;
use crate::tab::RepoTab;
use crate::test_support::fixture::Fixture;
use crate::test_support::geometry::{assert_rect, bounds, open_probe};
use crate::test_support::{DEFAULT_SIZE, init_app, mount_fixture};
use crate::theme::ActiveTokens as _;

// ----- helpers -----

/// Polls `done` while the shells and emulators work on their own threads, running the app between polls.
fn wait_until(
    cx: &mut TestAppContext,
    what: &str,
    mut done: impl FnMut(&mut TestAppContext) -> bool,
) {
    let started = Instant::now();
    loop {
        cx.run_until_parked();
        if done(cx) {
            return;
        }
        assert!(
            started.elapsed() < Duration::from_secs(40),
            "timed out: {what}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// A light shell for tests: Command Prompt on Windows, the login shell elsewhere.
fn test_shell(cx: &mut TestAppContext) {
    cx.update(|cx| {
        crate::settings::update(cx, |s| {
            s.terminal_shell = if cfg!(windows) {
                TerminalShell::Cmd
            } else {
                TerminalShell::Auto
            }
        })
    });
}

/// A window on a real repository with its terminal panel.
fn mount(
    cx: &mut TestAppContext,
) -> (
    Fixture,
    AnyWindowHandle,
    Entity<AppView>,
    Entity<RepoTab>,
    Entity<TerminalPanel>,
) {
    let fixture = Fixture::new();
    fixture.write("README.md", "terminal\n");
    fixture.commit_all("init");
    let (handle, app, tab) = mount_fixture(cx, &fixture, DEFAULT_SIZE);
    // Real shells and emulators wake the app from their own threads.
    cx.executor().allow_parking();
    test_shell(cx);
    let ctx = cx.update(|cx| tab.read(cx).ctx().clone());
    let panel = cx
        .update(|cx| panel_of(&ctx, cx))
        .expect("a repository with a worktree has a terminal panel");
    (fixture, handle, app, tab, panel)
}

fn render(cx: &mut TestAppContext, handle: AnyWindowHandle) {
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .expect("window");
}

fn toggle_panel(cx: &mut TestAppContext, handle: AnyWindowHandle, tab: &Entity<RepoTab>) {
    let ctx = cx.update(|cx| tab.read(cx).ctx().clone());
    cx.update_window(handle, |_, window, cx| toggle(&ctx, window, cx))
        .expect("window");
    render(cx, handle);
}

/// Shows the panel and waits for its first terminal's first frame.
fn show_and_wait(
    cx: &mut TestAppContext,
    handle: AnyWindowHandle,
    tab: &Entity<RepoTab>,
    panel: &Entity<TerminalPanel>,
) -> Entity<TerminalView> {
    toggle_panel(cx, handle, tab);
    wait_until(cx, "the first terminal should start and draw", |cx| {
        cx.update(|cx| {
            panel
                .read(cx)
                .active_terminal()
                .is_some_and(|view| view.read(cx).has_drawn())
        })
    });
    wait_until(cx, "the shell should print its prompt", |cx| {
        cx.update(|cx| {
            panel.read(cx).active_terminal().is_some_and(|view| {
                let view = view.read(cx);
                view.has_printed() && !view.screen_text().trim().is_empty()
            })
        })
    });
    render(cx, handle);
    cx.update(|cx| panel.read(cx).active_terminal().cloned())
        .expect("a terminal")
}

fn screen(cx: &mut TestAppContext, view: &Entity<TerminalView>) -> String {
    cx.update(|cx| view.read(cx).screen_text())
}

/// The command that prints `text` in the test shell.
fn echo(text: &str) -> String {
    format!("echo {text}\r")
}

fn send(cx: &mut TestAppContext, handle: AnyWindowHandle, view: &Entity<TerminalView>, text: &str) {
    cx.update_window(handle, |_, window, cx| {
        view.update(cx, |view, cx| view.send_text(text, window, cx))
    })
    .expect("window");
}

fn clipboard(cx: &mut TestAppContext) -> Option<String> {
    cx.update(|cx| cx.read_from_clipboard().and_then(|item| item.text()))
}

fn keystroke(text: &str) -> Keystroke {
    Keystroke::parse(text).expect("a valid keystroke")
}

const PLAIN: Style = Style {
    fg: RgbColor { r: 0, g: 0, b: 0 },
    bg: None,
    bold: false,
    underline: Underline::None,
    strikethrough: false,
    faint: false,
};

/// A row of `runs` side by side, each its text over that many columns.
fn row(runs: &[(&str, u16, Style)]) -> Row {
    let mut row = Row::default();
    let mut column = 0;
    for &(text, columns, style) in runs {
        let start = row.text.len();
        row.text.push_str(text);
        row.runs.push(Run {
            text: start..row.text.len(),
            columns: column..column + columns,
            style,
            link: None,
        });
        column += columns;
    }
    row
}

// ----- grid (21 §4.7) -----

#[test]
fn grid_size_from_bounds() {
    let cell = size(px(7.8), px(16.));
    let fitted = grid::grid_size(size(px(800.), px(206.)), cell);
    assert_eq!(
        (fitted.cols, fitted.rows),
        (102, 12),
        "whole cells only: 800 / 7.8 and 206 / 16, rounded down"
    );
    assert_eq!(
        (fitted.cell_width, fitted.cell_height),
        (8, 16),
        "programs that ask are told the cell's whole-pixel size"
    );
    let tiny = grid::grid_size(size(px(3.), px(2.)), cell);
    assert_eq!(
        (tiny.cols, tiny.rows),
        (2, 1),
        "a grid never shrinks below 2 columns and 1 row"
    );
    let unmeasured = grid::grid_size(size(px(100.), px(100.)), size(px(0.), px(0.)));
    assert_eq!(
        (unmeasured.cols, unmeasured.rows, unmeasured.cell_width),
        (2, 1, 1),
        "a font that measured nothing still gives a usable grid"
    );
}

#[test]
fn a_point_over_a_fractional_cell_lands_in_that_cell_on_the_surface() {
    let grid = Grid {
        origin: point(px(100.), px(50.)),
        cell: size(px(7.8), px(16.)),
    };
    let over_column_61 = point(px(100. + 7.8 * 60.5), px(58.));
    let at = grid.surface(over_column_61, view::OPENING);
    assert_eq!(
        (at.x / 8.) as u16 + 1,
        61,
        "7.8 px cells drawn map onto the 8 px cells the encoders count in, not column 59"
    );
}

#[test]
fn the_grid_stays_on_the_device_pixels_and_put_as_it_grows() {
    let placed = |x: f32, y: f32, height: f32, scale: f32| {
        let bounds = Bounds::new(point(px(x), px(y)), size(px(100.), px(height)));
        Grid::at(bounds, size(px(8.), px(16.)), scale).origin
    };
    assert_eq!(
        placed(10., 20., 49., 1.),
        placed(10., 20., 63., 1.),
        "what whole rows leave over goes below them, so dragging the panel's edge moves no text"
    );
    assert_eq!(
        placed(10.3, 20.2, 49., 2.),
        point(px(10.5), px(20.)),
        "bounds between device pixels start the grid on the nearest, so text stays crisp"
    );
}

#[test]
fn runs_show_the_same_text_however_wide_their_row_is() {
    let shown = |row: &Row| -> Vec<(u16, String)> {
        grid::shown_text(row)
            .map(|(column, text, _)| (column, text.to_owned()))
            .collect()
    };
    let number = Style {
        fg: RgbColor { r: 9, g: 9, b: 9 },
        ..PLAIN
    };
    let narrow = row(&[("  12 ", 5, number), ("fn main()  ", 11, PLAIN)]);
    let wide = row(&[("  12 ", 5, number), ("fn main()        ", 17, PLAIN)]);
    assert_eq!(
        shown(&narrow),
        [(2, "12".to_owned()), (5, "fn main()".to_owned())],
        "blanks are left out, each run starting where its text does"
    );
    assert_eq!(
        shown(&narrow),
        shown(&wide),
        "the padding of a wider row changes nothing shaped"
    );
    let underlined = Style {
        underline: Underline::Single,
        ..PLAIN
    };
    assert_eq!(
        shown(&row(&[("   ", 3, underlined)])),
        [(0, "   ".to_owned())],
        "underlined blanks show their line"
    );
}

#[test]
fn backgrounds_side_by_side_in_one_colour_are_joined() {
    let on = |r| Style {
        bg: Some(RgbColor { r, g: 0, b: 0 }),
        ..PLAIN
    };
    let row = row(&[
        ("use", 3, on(1)),
        (" crate", 6, on(1)),
        ("::", 2, PLAIN),
        ("x", 1, on(1)),
        ("y", 1, on(2)),
    ]);
    let joined: Vec<_> = grid::backgrounds(&row)
        .map(|(columns, color)| (columns, color.r))
        .collect();
    assert_eq!(
        joined,
        [(0..9, 1), (11..12, 1), (12..13, 2)],
        "one quad across runs of one colour, so no seam shows, but not across a gap"
    );
}

#[test]
fn the_link_under_a_point_is_the_one_its_cell_has() {
    let grid = Grid {
        origin: point(px(100.), px(50.)),
        cell: size(px(7.8), px(16.)),
    };
    let mut linked = row(&[("docs", 4, PLAIN), (" for more", 9, PLAIN)]);
    linked.runs[0].link = Some(Arc::from("https://example.com"));
    let rows = [Arc::new(Row::default()), Arc::new(linked)];
    let over = |column: f32, row: f32| point(px(100. + 7.8 * column), px(50. + 16. * row));
    assert_eq!(
        grid::link_at(&rows, grid, over(3.5, 1.5)).as_deref(),
        Some("https://example.com"),
        "the last of the link's cells"
    );
    assert_eq!(
        grid::link_at(&rows, grid, over(4.5, 1.5)),
        None,
        "the cell after it"
    );
    assert_eq!(
        grid::link_at(&rows, grid, over(-0.5, 1.5)),
        None,
        "left of the grid"
    );
}

#[test]
fn only_http_links_with_a_host_may_be_opened() {
    for uri in [
        "https://github.com/org/repo/pull/1",
        "http://example.com",
        "HTTPS://example.com:8443/a?b#c",
        "https://user@[::1]:8080/x",
    ] {
        assert!(grid::openable(uri), "{uri} is a web link");
    }
    for uri in [
        "file:///C:/Windows/System32/cmd.exe",
        r"\\host\share\x.exe",
        "ms-msdt:/id PCWDiagnostic",
        "search-ms:query=x&crumb=location:\\\\host\\share",
        "javascript:alert('http://x')",
        r"C:\x.exe",
        "https:///path-without-host",
        "https://",
        "http://host\\share",
        "https://exa mple.com",
        "https://example.com/\x07",
        "",
    ] {
        assert!(
            !grid::openable(uri),
            "{uri:?} could run a program or is no web link"
        );
    }
}

#[test]
fn a_link_that_may_not_be_opened_is_not_one_under_the_pointer() {
    let grid = Grid {
        origin: point(px(0.), px(0.)),
        cell: size(px(8.), px(16.)),
    };
    let mut linked = row(&[("https://github.com/org/repo/pull/1", 34, PLAIN)]);
    linked.runs[0].link = Some(Arc::from("file:///C:/Windows/System32/cmd.exe"));
    let rows = [Arc::new(linked)];
    assert_eq!(
        grid::link_at(&rows, grid, point(px(4.), px(8.))),
        None,
        "a file: target dressed as a web link is neither underlined nor followed"
    );
}

#[test]
fn colours_come_from_the_theme_tokens() {
    for tokens in [
        crate::theme::builtin::dark(),
        crate::theme::builtin::light(),
    ] {
        let colors = super::colors::TerminalColors::of(&tokens, &crate::theme::builtin::lanes());
        assert_eq!(
            colors.background,
            super::colors::rgb(tokens.background),
            "the terminal's ground is the theme's background"
        );
        assert_eq!(colors.colors.palette.len(), 16, "every ANSI colour is set");
        assert_eq!(
            colors.colors.palette[1].1,
            super::colors::rgb(tokens.error),
            "ANSI red is the theme's error colour"
        );
        assert_ne!(
            colors.foreground, colors.background,
            "text reads on the ground"
        );
    }
}

// ----- keys (21 §2.12, 18 §4.3) -----

#[test]
fn key_encoding_map() {
    use but_tty::{Key, Mods};
    assert_eq!(keys::key_for("a"), Key::A);
    assert_eq!(
        keys::key_for("Z"),
        Key::Z,
        "letters by their key, whatever the case"
    );
    assert_eq!(keys::key_for("7"), Key::Digit7);
    assert_eq!(keys::key_for("`"), Key::Backquote);
    assert_eq!(keys::key_for("enter"), Key::Enter);
    assert_eq!(keys::key_for("escape"), Key::Escape);
    assert_eq!(keys::key_for("pagedown"), Key::PageDown);
    assert_eq!(keys::key_for("left"), Key::ArrowLeft);
    assert_eq!(keys::key_for("f12"), Key::F12);
    assert_eq!(
        keys::key_for("f13"),
        Key::Unidentified,
        "past F12 is unknown"
    );

    let typed = |text: &str, key_char: &str| {
        let mut stroke = keystroke(text);
        stroke.key_char = Some(key_char.to_owned());
        stroke
    };
    assert_eq!(
        keys::route(&typed("a", "a"), false),
        KeyRoute::Text,
        "typed characters come through the input handler"
    );
    assert_eq!(
        keys::route(&typed("shift-a", "A"), false),
        KeyRoute::Text,
        "shifted characters too"
    );
    assert_eq!(
        keys::route(&typed("ctrl-alt-q", "@"), true),
        KeyRoute::Text,
        "an AltGr character is text, not Ctrl+Alt+Q"
    );
    assert_eq!(
        keys::route(&typed("6", "^"), true),
        KeyRoute::Text,
        "a dead key waits for the input handler's composed character"
    );
    assert_eq!(
        keys::route(&keystroke("ctrl-d"), false),
        KeyRoute::Encode {
            key: Key::D,
            mods: Mods::CTRL,
            text: None,
            unshifted: Some('d'),
        },
        "Ctrl+letter is encoded for the program"
    );
    assert_eq!(
        keys::route(&typed("alt-x", "x"), false),
        KeyRoute::Encode {
            key: Key::X,
            mods: Mods::ALT,
            text: Some("x".into()),
            unshifted: Some('x'),
        },
        "Alt+letter is encoded (ESC x), as Windows sends no character for it"
    );
    assert_eq!(
        keys::route(&keystroke("shift-tab"), false),
        KeyRoute::Encode {
            key: Key::Tab,
            mods: Mods::SHIFT,
            text: None,
            unshifted: None,
        },
        "Shift+Tab reaches the shell (completion backwards)"
    );
    assert!(
        matches!(
            keys::route(&keystroke("space"), false),
            KeyRoute::Encode {
                key: Key::Space,
                unshifted: Some(' '),
                ..
            }
        ),
        "a space without its character is still the space key"
    );
}

#[cfg(not(target_os = "macos"))]
#[test]
fn windows_chords_copy_paste_routes() {
    assert_eq!(
        keys::route(&keystroke("ctrl-c"), false),
        KeyRoute::CopyOrInterrupt,
        "Ctrl+C copies a selection, else interrupts"
    );
    assert_eq!(
        keys::route(&keystroke("ctrl-shift-c"), false),
        KeyRoute::Copy
    );
    assert_eq!(
        keys::route(&keystroke("ctrl-shift-v"), false),
        KeyRoute::Paste
    );
    assert_eq!(
        keys::route(&keystroke("ctrl-insert"), false),
        KeyRoute::Copy
    );
    assert_eq!(
        keys::route(&keystroke("shift-insert"), false),
        KeyRoute::Paste
    );
    assert!(
        matches!(
            keys::route(&keystroke("ctrl-v"), false),
            KeyRoute::Encode { .. }
        ),
        "Ctrl+V alone is the program's (vim's visual block)"
    );
}

/// The probe of 21 §2.12: in a `Terminal` element inside the repository frame, app shortcuts reach the key handler
/// instead of their actions, but Ctrl+` still toggles the panel.
#[gpui_kit::test]
fn terminal_context_noaction_swallows_app_keys(cx: &mut TestAppContext) {
    init_app(cx);
    let keys_seen: Rc<RefCell<Vec<String>>> = Rc::default();
    let actions_run: Rc<RefCell<Vec<&'static str>>> = Rc::default();
    let focus = cx.update(|cx| cx.focus_handle());
    let (handle, _) = {
        let keys_seen = keys_seen.clone();
        let actions_run = actions_run.clone();
        let focus = focus.clone();
        open_probe(cx, 400., 300., move |_, _| {
            let (stash, close, refresh, toggle) = (
                actions_run.clone(),
                actions_run.clone(),
                actions_run.clone(),
                actions_run.clone(),
            );
            let keys_seen = keys_seen.clone();
            div()
                .size_full()
                .key_context(crate::actions::REPO_CONTEXT)
                .on_action(move |_: &crate::actions::Stash, _, _| stash.borrow_mut().push("stash"))
                .on_action(move |_: &crate::actions::CloseLastView, _, _| {
                    close.borrow_mut().push("close")
                })
                .on_action(move |_: &crate::actions::Refresh, _, _| {
                    refresh.borrow_mut().push("refresh")
                })
                .on_action(move |_: &crate::actions::ToggleTerminal, _, _| {
                    toggle.borrow_mut().push("toggle")
                })
                .child(
                    div()
                        .id("probe-terminal")
                        .size_full()
                        .key_context(TERMINAL_CONTEXT)
                        .track_focus(&focus)
                        .on_key_down(move |event, _, cx| {
                            keys_seen.borrow_mut().push(event.keystroke.unparse());
                            cx.stop_propagation();
                        }),
                )
                .into_any_element()
        })
    };
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.focus(&focus, cx);
        window.render_frame(cx);
        // Stash and Refresh are bound on the primary modifier: Ctrl, Cmd on macOS.
        for key in [
            "secondary-s",
            "escape",
            "f5",
            "tab",
            "shift-tab",
            "secondary-r",
            "ctrl-`",
        ] {
            window.press(key, cx);
        }
    })
    .expect("window");
    let (stash_key, refresh_key) = if cfg!(target_os = "macos") {
        ("cmd-s", "cmd-r")
    } else {
        ("ctrl-s", "ctrl-r")
    };
    assert_eq!(
        keys_seen.borrow().as_slice(),
        [stash_key, "escape", "f5", "tab", "shift-tab", refresh_key],
        "app shortcuts and Tab reach the terminal's key handler"
    );
    assert_eq!(
        actions_run.borrow().as_slice(),
        ["toggle"],
        "no app action runs from inside the terminal but the panel toggle"
    );
}

// ----- shells (18 §5.5) -----

#[cfg(windows)]
#[test]
fn shell_detection_order() {
    let root = PathBuf::from(r"C:\Windows");
    let program_files = PathBuf::from(r"C:\Program Files");
    let git = PathBuf::from(r"C:\Tools\Git");
    let everything = [
        program_files.join(r"PowerShell\7\pwsh.exe"),
        root.join(r"System32\WindowsPowerShell\v1.0\powershell.exe"),
        root.join(r"System32\cmd.exe"),
        root.join(r"System32\wsl.exe"),
        git.join(r"cmd\git.exe"),
        git.join(r"bin\bash.exe"),
    ];
    let probe_with = |files: Vec<PathBuf>| {
        let exists = move |path: &Path| files.iter().any(|file| file == path);
        shell::detect(&Probe {
            path: Some(std::env::join_paths([git.join("cmd")]).expect("a PATH")),
            program_files: Some(program_files.clone()),
            system_root: Some(root.clone()),
            comspec: None,
            login_shell: None,
            exists: &exists,
        })
    };
    let all = probe_with(everything.to_vec());
    let choices: Vec<&TerminalShell> = all.iter().map(|shell| &shell.choice).collect();
    assert_eq!(
        choices,
        [
            &TerminalShell::Pwsh,
            &TerminalShell::WindowsPowerShell,
            &TerminalShell::Cmd,
            &TerminalShell::GitBash,
            &TerminalShell::Wsl,
        ],
        "the pickers list PowerShell 7, Windows PowerShell, Command Prompt, Git Bash, WSL"
    );
    let bash = all
        .iter()
        .find(|shell| shell.choice == TerminalShell::GitBash)
        .expect("Git Bash");
    assert_eq!(
        PathBuf::from(&bash.spec.program),
        git.join(r"bin\bash.exe"),
        "Git Bash is found next to the git.exe on PATH"
    );
    assert!(
        bash.spec
            .env
            .iter()
            .any(|(name, value)| name == "CHERE_INVOKING" && value == "1"),
        "Git Bash stays in the repository's folder"
    );
    assert_eq!(
        PathBuf::from(&shell::automatic(&all).program),
        program_files.join(r"PowerShell\7\pwsh.exe"),
        "Automatic prefers PowerShell 7"
    );
    let without_pwsh = probe_with(everything[1..].to_vec());
    assert_eq!(
        PathBuf::from(&shell::automatic(&without_pwsh).program),
        root.join(r"System32\WindowsPowerShell\v1.0\powershell.exe"),
        "then Windows PowerShell"
    );
    let only_cmd = probe_with(vec![root.join(r"System32\cmd.exe")]);
    assert_eq!(
        PathBuf::from(&shell::automatic(&only_cmd).program),
        root.join(r"System32\cmd.exe"),
        "then Command Prompt"
    );
    assert!(
        shell::resolve(&TerminalShell::GitBash, &only_cmd)
            .expect_err("Git Bash is not installed")
            .contains("Git Bash"),
        "a chosen shell that is missing is named"
    );
}

#[cfg(unix)]
#[test]
fn shell_detection_order() {
    let exists = |_: &Path| true;
    let found: Vec<shell::DetectedShell> = shell::detect(&Probe {
        path: None,
        program_files: None,
        system_root: None,
        comspec: None,
        login_shell: Some("/usr/bin/fish".into()),
        exists: &exists,
    });
    assert_eq!(
        found.len(),
        1,
        "the login shell is the one choice off Windows"
    );
    assert_eq!(
        found[0].spec.program, "/usr/bin/fish",
        "$SHELL, as a login shell"
    );
    assert_eq!(found[0].spec.args, ["-l"]);
}

#[test]
fn custom_shell_command_lines() {
    assert_eq!(
        shell::split_windows(r#""C:\Program Files\nu\nu.exe" -l --config "a b\" c""#),
        [r"C:\Program Files\nu\nu.exe", "-l", "--config", r#"a b" c"#],
        "quotes group, backslashes stay unless they escape a quote"
    );
    assert_eq!(
        shell::split_windows(r"C:\tools\nu.exe   -l"),
        [r"C:\tools\nu.exe", "-l"]
    );
    let custom = shell::custom("pwsh -NoLogo").expect("a custom shell");
    assert_eq!(
        custom.kind,
        ShellKind::PowerShell,
        "the kind follows the program's name"
    );
    assert!(
        shell::custom("   ").is_err(),
        "an empty command line names no shell"
    );
}

#[test]
fn dropped_paths_are_quoted_for_the_shell() {
    let paths = [r"C:\Users\me\notes.txt", r"C:\My Files\it's here.md"];
    assert_eq!(
        shell::quoted(ShellKind::PowerShell, paths),
        r"C:\Users\me\notes.txt 'C:\My Files\it''s here.md'",
        "PowerShell: single quotes, a quote doubled"
    );
    assert_eq!(
        shell::quoted(ShellKind::Cmd, paths),
        r#"C:\Users\me\notes.txt "C:\My Files\it's here.md""#,
        "Command Prompt: double quotes"
    );
    assert_eq!(
        shell::quoted(ShellKind::Posix, ["/tmp/a b", "/tmp/$HOME;rm"]),
        r"'/tmp/a b' '/tmp/$HOME;rm'",
        "POSIX shells: single quotes"
    );
}

#[test]
fn powershell_quoting_doubles_typographic_single_quotes_too() {
    assert_eq!(
        shell::quoted(ShellKind::PowerShell, ["x\u{2019}; calc; \u{2019}.txt"]),
        "'x\u{2019}\u{2019}; calc; \u{2019}\u{2019}.txt'",
        "a right single quote would end the string and run `calc`"
    );
    for quote in ['\'', '\u{2018}', '\u{2019}', '\u{201A}', '\u{201B}'] {
        assert_eq!(
            shell::quoted(ShellKind::PowerShell, [format!("a{quote}b")]),
            format!("'a{quote}{quote}b'"),
            "PowerShell takes {quote:?} as a single quote, escaped by doubling it"
        );
    }
}

// ----- panel (21 §5.2, §8.2) -----

#[gpui_kit::test]
fn panel_geometry_and_resize_clamp(cx: &mut TestAppContext) {
    let (_fixture, handle, _app, tab, panel) = mount(cx);
    render(cx, handle);
    cx.update_window(handle, |_, window, _| {
        assert_rect(window, ids::PANEL, 0., 720. - 33., 1280., 0.);
    })
    .expect("window");
    let _view = show_and_wait(cx, handle, &tab, &panel);
    let height = metrics::TERMINAL_DEFAULT_H;
    let top = 720. - metrics::BOTTOM_BAR_H - metrics::FRAME_DIVIDER_H - (height + 16.);
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        assert_rect(window, ids::PANEL, 0., top, 1280., height + 16.);
        assert_rect(window, ids::RESIZER, 0., top, 1280., 8.);
        assert_rect(window, ids::BOX, 8., top + 8., 1264., height);
        assert_rect(window, ids::HEADER, 8., top + 8., 1264., 34.);
        assert_rect(window, ids::BODY, 8., top + 42., 1264., height - 34.);
        let tab0 = bounds(window, (ids::TAB, 0usize));
        assert!(
            (tab0.origin.x.as_f32() - 8.).abs() < 0.5 && tab0.size.height.as_f32() == 34.,
            "the first tab starts the header and is as tall as it"
        );
        assert!(
            tab0.size.width.as_f32() >= 120. && tab0.size.width.as_f32() <= 240.,
            "a tab is 120 to 240 dp wide"
        );
        assert_rect(window, ids::NEW, tab0.right().as_f32(), top + 8., 34., 34.);
        assert_rect(window, ids::HIDE, 1280. - 8. - 8. - 34., top + 8., 34., 34.);
        assert_rect(window, ids::VIEW, 8., top + 42., 1264., height - 34.);

        // Up past the limit: the split keeps the right pane's fixed content (276 dp) under the toolbar.
        let grab = bounds(window, ids::RESIZER).center();
        window.drag(grab, point(grab.x, px(1.)), cx);
        window.render_frame(cx);
        let max = 720. - 36. - 64. - 276. - 16. - 1. - 32.;
        assert_rect(window, ids::PANEL, 0., 36. + 64. + 276., 1280., max + 16.);
    })
    .expect("window");
    let persisted = cx.update(|cx| crate::settings::state::get(cx).terminal_height);
    assert_eq!(
        persisted, 295.,
        "the height is persisted when the drag ends"
    );
    cx.update_window(handle, |_, window, cx| {
        let grab = bounds(window, ids::RESIZER).center();
        window.drag(grab, point(grab.x, px(719.)), cx);
        window.render_frame(cx);
        assert_rect(
            window,
            ids::PANEL,
            0.,
            720. - 33. - (metrics::TERMINAL_MIN_H + 16.),
            1280.,
            metrics::TERMINAL_MIN_H + 16.,
        );
    })
    .expect("window");
    cx.update(|cx| {
        assert_eq!(
            crate::settings::state::get(cx).terminal_height,
            metrics::TERMINAL_MIN_H,
            "down past the limit stops at the minimum"
        );
    });
}

#[gpui_kit::test]
fn the_tallest_panel_leaves_the_status_headers_and_actions(cx: &mut TestAppContext) {
    let fixture = Fixture::new();
    fixture.write("README.md", "terminal\n");
    fixture.write("b.txt", "b\n");
    fixture.commit_all("init");
    fixture.write("README.md", "staged\n");
    fixture.git(&["add", "README.md"]);
    fixture.write("b.txt", "unstaged\n");
    // A maximized 1080p window at 150 %.
    let (handle, _app, tab) =
        mount_fixture(cx, &fixture, crate::test_support::window_size(1280., 668.));
    cx.executor().allow_parking();
    test_shell(cx);
    let ctx = cx.update(|cx| tab.read(cx).ctx().clone());
    let panel = cx
        .update(|cx| panel_of(&ctx, cx))
        .expect("a terminal panel");
    show_and_wait(cx, handle, &tab, &panel);
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let grab = bounds(window, ids::RESIZER).center();
        window.drag(grab, point(grab.x, px(1.)), cx);
        window.render_frame(cx);
        let panel_top = bounds(window, ids::PANEL).top().as_f32();
        assert!(
            (panel_top - (36. + 64. + 276.)).abs() <= 0.5,
            "the split keeps 276 dp, got the panel at {panel_top}"
        );
        let status = crate::status_pane::ids::STAGED;
        let unstaged = crate::status_pane::ids::UNSTAGED;
        for header in [status.header, unstaged.header] {
            let b = bounds(window, header);
            assert!(
                (b.size.height.as_f32() - 34.).abs() <= 0.5,
                "08 §0.5: the {header} bar keeps its 34 dp, got {b:?}"
            );
        }
        for action in [status.action, unstaged.action] {
            let b = bounds(window, action);
            assert!(
                b.size.height.as_f32() >= 20. && b.bottom().as_f32() <= panel_top,
                "{action} (Unstage all / Stage all) is visible above the panel, got {b:?}"
            );
        }
        let commit = bounds(window, crate::status_pane::commit_box::COMMIT_BOX);
        assert!(
            (commit.size.height.as_f32() - 192.).abs() <= 0.5
                && commit.bottom().as_f32() <= panel_top,
            "the commit box keeps its 192 dp above the panel, got {commit:?}"
        );
        window.click(unstaged.action, cx);
    })
    .expect("window");
}

#[gpui_kit::test]
fn toggle_focus_rules(cx: &mut TestAppContext) {
    let (_fixture, handle, _app, tab, panel) = mount(cx);
    let view = show_and_wait(cx, handle, &tab, &panel);
    let ctx = cx.update(|cx| tab.read(cx).ctx().clone());
    cx.update_window(handle, |_, window, cx| {
        assert!(
            view.read(cx).focus_handle(cx).is_focused(window),
            "showing focuses the terminal"
        );
        let state = toolbar_button_state(&ctx, cx);
        assert!(state.enabled);
        assert_eq!(
            state.tooltip, HIDE_TOOLTIP,
            "the button offers to hide the panel"
        );
        assert!(
            ctx.ui.read(cx).terminal_visible,
            "the tab's UI state follows the panel"
        );
        if cfg!(windows) {
            assert_eq!(
                view.read(cx).title(),
                "Command Prompt",
                "the tab names the shell rather than the path the pseudoconsole titles it with"
            );
        }
    })
    .expect("window");
    toggle_panel(cx, handle, &tab);
    cx.update_window(handle, |_, window, cx| {
        let focus = tab.read(cx).focus.clone();
        assert!(
            focus.is_focused(window),
            "hiding gives the keys back to the repository frame"
        );
        assert!(!panel.read(cx).is_visible());
        assert_eq!(
            panel.read(cx).terminals().count(),
            1,
            "the terminal stays alive while hidden"
        );
        assert_eq!(toolbar_button_state(&ctx, cx).tooltip, SHOW_TOOLTIP);
    })
    .expect("window");
    toggle_panel(cx, handle, &tab);
    cx.update_window(handle, |_, window, cx| {
        let shown = panel
            .read(cx)
            .active_terminal()
            .cloned()
            .expect("the terminal");
        assert_eq!(
            shown, view,
            "showing again brings back the same terminal, not a new one"
        );
        assert!(
            view.read(cx).focus_handle(cx).is_focused(window),
            "and focuses it"
        );
    })
    .expect("window");
    // Typing reaches the shell through the input handler (and the Enter key through the encoder).
    cx.update_window(handle, |_, window, cx| {
        window.input("echo typed-in", cx);
        window.press("enter", cx);
    })
    .expect("window");
    wait_until(cx, "what was typed runs in the shell", |cx| {
        screen(cx, &view)
            .lines()
            .any(|line| line.trim() == "typed-in")
    });
}

/// The last shell of a panel on a background repository tab exits: the panel hides without taking the keys from the
/// selected tab.
#[gpui_kit::test]
fn a_panel_hiding_on_its_own_leaves_the_keys_where_they_are(cx: &mut TestAppContext) {
    let (_fixture, handle, app, tab, panel) = mount(cx);
    let view = show_and_wait(cx, handle, &tab, &panel);
    // Typed while the tab is selected (typing takes the keys); the shell exits a moment after the switch below.
    let exit_soon = if cfg!(windows) {
        "ping -n 3 127.0.0.1 > nul & exit\r"
    } else {
        "sleep 2; exit\r"
    };
    send(cx, handle, &view, exit_soon);
    let other = cx
        .update_window(handle, |_, window, cx| {
            let other = cx.new(|cx| {
                RepoTab::new(
                    crate::tab::TabId::next(),
                    crate::backend::handle::TabBackend::Inert(Arc::new(
                        crate::test_support::InertBackend::default(),
                    )),
                    "other".into(),
                    PathBuf::from("other"),
                    crate::tab::TabOptions {
                        watch: false,
                        prompter: crate::tab::PrompterMode::Refuse,
                    },
                    window,
                    cx,
                )
            });
            app.update(cx, |app, cx| {
                app.add_repo_tab(other.clone(), true, window, cx)
            });
            other
        })
        .expect("window");
    render(cx, handle);
    let other_focus = cx.update(|cx| other.read(cx).focus.clone());
    cx.update_window(handle, |_, window, cx| window.focus(&other_focus, cx))
        .expect("window");
    render(cx, handle);
    cx.update(|cx| {
        assert!(
            !view.read(cx).is_shown(),
            "a terminal of a background tab builds no frames"
        )
    });
    wait_until(cx, "the shell exits and the panel hides", |cx| {
        cx.update(|cx| !panel.read(cx).is_visible())
    });
    render(cx, handle);
    cx.update_window(handle, |_, window, _| {
        assert!(
            other_focus.is_focused(window),
            "the selected tab keeps the keys"
        );
    })
    .expect("window");
}

#[test]
fn the_tab_names_the_shell_in_place_of_its_path() {
    let program = r"C:\Program Files\PowerShell\7\pwsh.exe";
    let name: gpui_kit::SharedString = "PowerShell 7".into();
    let title = |shown: &str| super::view::shown_title(shown, program, &name).to_string();
    assert_eq!(
        title(r"C:\PROGRAM FILES\PowerShell\7\pwsh.exe"),
        "PowerShell 7",
        "the path a pseudoconsole titles the terminal with reads as the shell's name"
    );
    assert_eq!(
        title(r"C:\Program Files\PowerShell\7\pwsh.exe - git log"),
        "PowerShell 7 - git log",
        "what follows the path stays"
    );
    assert_eq!(title("  "), "PowerShell 7", "no title yet: the name");
    assert_eq!(
        title("vim notes.txt"),
        "vim notes.txt",
        "a title the shell or a program sets is shown as it is"
    );
    assert_eq!(
        title(r"C:\Program Files\PowerShell\7\pwsh.exe.bak"),
        r"C:\Program Files\PowerShell\7\pwsh.exe.bak",
        "only the whole path is replaced"
    );
}

#[gpui_kit::test]
fn windows_chords_copy_paste(cx: &mut TestAppContext) {
    let (_fixture, handle, _app, tab, panel) = mount(cx);
    let view = show_and_wait(cx, handle, &tab, &panel);
    send(cx, handle, &view, &echo("copy-me"));
    wait_until(cx, "the shell prints", |cx| {
        screen(cx, &view).contains("copy-me")
    });
    cx.update(|cx| cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string("before".into())));
    cx.update_window(handle, |_, window, cx| window.press("ctrl-c", cx))
        .expect("window");
    cx.run_until_parked();
    assert_eq!(
        clipboard(cx).as_deref(),
        Some("before"),
        "Ctrl+C without a selection copies nothing (it interrupts the shell)"
    );
    cx.update(|cx| view.update(cx, |view, cx| view.select_all(cx)));
    cx.update_window(handle, |_, window, cx| window.press("ctrl-c", cx))
        .expect("window");
    cx.run_until_parked();
    assert!(
        clipboard(cx).is_some_and(|text| text.contains("copy-me")),
        "Ctrl+C with a selection copies it"
    );
    cx.update(|cx| {
        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string("insert-copied".into()))
    });
    cx.update_window(handle, |_, window, cx| window.press("ctrl-insert", cx))
        .expect("window");
    cx.run_until_parked();
    assert!(
        clipboard(cx).is_some_and(|text| text.contains("copy-me")),
        "Ctrl+Insert copies the selection too"
    );
    // The clipboard holds a command without Enter: a shell with line editing (bash 5.1+, zsh) turns bracketed paste
    // on, and a pasted line is then inserted, not run. So the pasted command must show on the prompt line first;
    // Enter then runs it, which proves the shell got the text as its input.
    let paste_and_run = |cx: &mut TestAppContext, key: &str, word: &str| {
        let command = format!("echo {word}");
        cx.update(|cx| cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(command.clone())));
        cx.update_window(handle, |_, window, cx| window.press(key, cx))
            .expect("window");
        wait_until(
            cx,
            &format!("{key} pastes the clipboard into the shell"),
            |cx| screen(cx, &view).contains(&command),
        );
        send(cx, handle, &view, "\r");
        wait_until(cx, &format!("the command pasted by {key} runs"), |cx| {
            screen(cx, &view).lines().any(|line| line.trim() == word)
        });
    };
    paste_and_run(cx, "ctrl-shift-v", "pasted-by-chord");
    paste_and_run(cx, "shift-insert", "pasted-by-insert");
}

#[gpui_kit::test]
fn hidden_panel_stops_frames(cx: &mut TestAppContext) {
    let (_fixture, handle, _app, tab, panel) = mount(cx);
    let first = show_and_wait(cx, handle, &tab, &panel);
    cx.update_window(handle, |_, window, cx| {
        panel.update(cx, |panel, cx| panel.open_terminal(None, window, cx))
    })
    .expect("window");
    wait_until(cx, "a second terminal opens", |cx| {
        cx.update(|cx| panel.read(cx).terminals().count() == 2)
    });
    let second = cx
        .update(|cx| panel.read(cx).active_terminal().cloned())
        .expect("terminal");
    cx.update(|cx| {
        assert!(second.read(cx).is_shown(), "the selected terminal draws");
        assert!(
            !first.read(cx).is_shown(),
            "a terminal in another tab of the panel builds no frames"
        );
    });
    toggle_panel(cx, handle, &tab);
    cx.update(|cx| {
        assert!(
            panel
                .read(cx)
                .terminals()
                .all(|view| !view.read(cx).is_shown()),
            "a hidden panel's terminals build no frames"
        );
    });
    send(cx, handle, &second, &echo("while-hidden"));
    // The shell answers at once; give it time to print what it would have drawn.
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(1500) {
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        !screen(cx, &second).contains("while-hidden"),
        "out of sight the shell is read but no frame is drawn"
    );
    toggle_panel(cx, handle, &tab);
    wait_until(cx, "shown again, what it printed is drawn", |cx| {
        screen(cx, &second)
            .lines()
            .any(|line| line.trim() == "while-hidden")
    });
}

/// The command that reports "working" (OSC 7501) and keeps running, as a long build would.
fn working_command() -> String {
    if cfg!(windows) {
        "powershell -NoProfile -Command \"[Console]::Write([char]27 + ']7501;state=working' + [char]27 + [char]92); Start-Sleep 60\"\r".to_owned()
    } else {
        "sh -c \"printf '\\033]7501;state=working\\033\\\\'; sleep 60\"\r".to_owned()
    }
}

/// A long-running program that reports nothing, as a plain build or an editor.
fn plain_long_command() -> String {
    if cfg!(windows) {
        "ping -n 60 127.0.0.1\r".to_owned()
    } else {
        "sleep 60\r".to_owned()
    }
}

#[gpui_kit::test]
fn close_tab_with_a_plain_long_program_confirms(cx: &mut TestAppContext) {
    let (_fixture, handle, _app, tab, panel) = mount(cx);
    let view = show_and_wait(cx, handle, &tab, &panel);
    let ctx = cx.update(|cx| tab.read(cx).ctx().clone());
    assert!(
        cx.update(|cx| confirm_close(&ctx, cx)).is_none(),
        "an idle shell closes without asking"
    );
    send(cx, handle, &view, &plain_long_command());
    wait_until(
        cx,
        "a running program that reports no status still makes closing ask",
        |cx| cx.update(|cx| confirm_close(&ctx, cx)).is_some(),
    );
}

#[gpui_kit::test]
fn close_tab_with_working_shell_confirms(cx: &mut TestAppContext) {
    let (_fixture, handle, app, tab, panel) = mount(cx);
    let view = show_and_wait(cx, handle, &tab, &panel);
    let ctx = cx.update(|cx| tab.read(cx).ctx().clone());
    assert!(
        cx.update(|cx| confirm_close(&ctx, cx)).is_none(),
        "an idle shell closes without asking"
    );
    send(cx, handle, &view, &working_command());
    wait_until(cx, "the program reports it is working", |cx| {
        cx.update(|cx| panel.read(cx).aggregate_status(cx) == Some(but_tty::Status::Working))
    });
    render(cx, handle);
    cx.update(|cx| {
        let state = toolbar_button_state(&ctx, cx);
        assert_eq!(
            state.status_dot,
            Some(cx.tokens().primary_variant),
            "the toolbar button shows a working terminal's dot"
        );
        let request = confirm_close(&ctx, cx).expect("closing asks while a program works");
        assert_eq!(request.confirm_label.as_ref(), "Close tab");
    });
    let id = ctx.id;
    let weak_panel = panel.downgrade();
    let weak_view = view.downgrade();
    drop((panel, view, tab, ctx));
    cx.update_window(handle, |_, window, cx| {
        app.update(cx, |app, cx| app.close_tab(id, window, cx));
        window.render_frame(cx);
        window.click("dialog-primary", cx);
    })
    .expect("window");
    render(cx, handle);
    cx.update(|cx| {
        assert!(
            app.read(cx).tabs().iter().all(|slot| slot.id != id),
            "confirming closes the tab"
        );
    });
    wait_until(cx, "the tab's terminal is let go, ending its shell", |_| {
        weak_view.upgrade().is_none() && weak_panel.upgrade().is_none()
    });
}

// ----- settings (21 §5.4) -----

#[gpui_kit::test]
fn settings_page_shell_custom_and_font_size(cx: &mut TestAppContext) {
    init_app(cx);
    let (handle, _app) = crate::test_support::open_app(cx, DEFAULT_SIZE);
    cx.update_window(handle, |_, window, cx| {
        crate::dialogs::settings::open(
            window,
            cx,
            Some(crate::dialogs::settings::SettingsPage::Terminal),
        );
        window.render_frame(cx);
        window.render_frame(cx);
    })
    .expect("window");
    use crate::dialogs::settings::ids as sids;
    use crate::dialogs::settings::terminal::{CUSTOM, FONT_SIZE, SHELL, shell_options};
    cx.update_window(handle, |_, window, cx| {
        for key in [SHELL, CUSTOM, FONT_SIZE] {
            window.find(sids::row(key));
        }
        assert!(settings_available(cx), "the terminal build shows its rows");
        window.click(sids::select(SHELL), cx);
        window.render_frame(cx);
        window.find(sids::select_popup(SHELL));
        let options = shell_options();
        assert_eq!(
            options.first(),
            Some(&TerminalShell::Auto),
            "Automatic comes first"
        );
        assert!(
            matches!(options.last(), Some(TerminalShell::Custom(_))),
            "Custom comes last"
        );
        // The option right under the box lies over the Custom row: the press must reach the option.
        window.click(sids::select_option(SHELL, 1), cx);
        window.render_frame(cx);
        assert_eq!(
            crate::settings::get(cx).terminal_shell,
            options[1],
            "the option over the next row takes the click"
        );
        window.click(sids::select(SHELL), cx);
        window.render_frame(cx);
        window.click(sids::select_option(SHELL, options.len() - 1), cx);
        window.render_frame(cx);
    })
    .expect("window");
    cx.run_until_parked();
    assert!(
        matches!(
            cx.update(|cx| crate::settings::get(cx).terminal_shell.clone()),
            TerminalShell::Custom(_)
        ),
        "choosing Custom stores it"
    );
    cx.update_window(handle, |_, window, cx| {
        window.click(sids::field(CUSTOM), cx);
        window.render_frame(cx);
        window.input("nu -l", cx);
        window.render_frame(cx);
    })
    .expect("window");
    cx.run_until_parked();
    assert_eq!(
        cx.update(|cx| crate::settings::get(cx).terminal_shell.clone()),
        TerminalShell::Custom("nu -l".into()),
        "the custom command line is stored as typed"
    );
    cx.update_window(handle, |_, window, cx| {
        window.click(sids::field(FONT_SIZE), cx);
        window.render_frame(cx);
        window.press("secondary-a", cx);
        window.input("16", cx);
        window.render_frame(cx);
    })
    .expect("window");
    cx.run_until_parked();
    assert_eq!(
        cx.update(|cx| crate::settings::get(cx).terminal_font_size),
        16.,
        "the font size is stored"
    );
    cx.update_window(handle, |_, window, cx| {
        window.press("secondary-a", cx);
        window.input("99", cx);
        window.render_frame(cx);
    })
    .expect("window");
    cx.run_until_parked();
    assert_eq!(
        cx.update(|cx| crate::settings::get(cx).terminal_font_size),
        16.,
        "a size outside 6..48 is refused"
    );
}

#[test]
fn bindings_neutralise_every_ancestor_key_but_the_toggle() {
    let bound: Vec<String> = keys::bindings()
        .iter()
        .map(|binding| {
            binding
                .keystrokes()
                .iter()
                .map(|k| k.unparse())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    assert!(
        !bound.iter().any(|key| key == "ctrl-`"),
        "Ctrl+` keeps toggling the panel"
    );
    assert!(bound.iter().any(|key| key == "tab") && bound.iter().any(|key| key == "shift-tab"));
    assert_eq!(
        bound.len(),
        crate::actions::ANCESTOR_KEYS.len() - 1 + 2,
        "one NoAction per ancestor key but the toggle, plus Tab and Shift+Tab"
    );
}
