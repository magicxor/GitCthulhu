//! App-level verbs: tabs, repositories, theme and settings, window size, keys and actions.

use std::path::PathBuf;

use gpui_kit::component::WindowExt as _;
use gpui_kit::{App, Keystroke, Window, px, size};

use super::{active_ctx, app_view};
use crate::backend::RefreshSet;
use crate::qa::monitor;
use crate::qa::session;
use crate::qa::verbs::{Args, Verb, register_verb};
use crate::settings::{self, DiffViewMode, LinesHeight};
use crate::theme::ThemeKind;

/// Registers the verbs of this file.
pub(super) fn register(cx: &mut App) {
    register_verb(
        cx,
        Verb::new(
            "new tab",
            "new tab",
            "Opens a new empty tab (the welcome page)",
            |window, cx, _| {
                let view = app_view(cx)?;
                view.update(cx, |view, cx| view.new_empty_tab(window, cx));
                Ok(String::new())
            },
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "open repository",
            "open repository <path | fixture name>",
            "Opens a repository inside the sandbox in the selected tab (a bare name means $REPOS/<name>)",
            open_repository,
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "select tab",
            "select tab <number from 1 | title>",
            "Selects a tab by position or by (part of) its title",
            select_tab,
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "close tab",
            "close tab [number from 1]",
            "Closes the selected tab (or the numbered one)",
            close_tab,
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "reload",
            "reload",
            "Reloads everything of the selected repository (F5)",
            |window, cx, _| {
                let ctx = active_ctx(cx)?;
                ctx.with_tab(window, cx, |tab, _, cx| tab.reload(RefreshSet::ALL, cx));
                Ok(String::new())
            },
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "resize",
            "resize <width>x<height>",
            "Sets the window's content size in logical pixels (the window must not be maximized)",
            |window, _, args| {
                let text = args.text(0, "size such as 1280x720")?;
                let (w, h) = parse_size(&text)?;
                window.resize(size(px(w), px(h)));
                Ok(format!("{w}x{h}"))
            },
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "set theme",
            "set theme dark|light",
            "Switches the built-in theme",
            |_, cx, args| {
                let kind = match args.text(0, "dark or light")?.to_lowercase().as_str() {
                    "dark" => ThemeKind::Dark,
                    "light" => ThemeKind::Light,
                    other => return Err(format!("unknown theme '{other}' (dark or light)")),
                };
                settings::update(cx, |s| s.theme = kind);
                Ok(format!("{kind:?}"))
            },
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "set scale",
            "set scale <0.75..3.0>",
            "Sets the UI scale of the Appearance settings",
            |_, cx, args| {
                let scale: f32 = args.number(0, "scale")?;
                settings::update(cx, |s| s.scale = scale);
                Ok(format!("{scale}"))
            },
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "set diff",
            "set diff split|unified",
            "Chooses the diff view mode",
            |_, cx, args| {
                let mode = match args.text(0, "split or unified")?.to_lowercase().as_str() {
                    "split" => DiffViewMode::Split,
                    "unified" => DiffViewMode::Unified,
                    other => return Err(format!("unknown diff mode '{other}' (split or unified)")),
                };
                settings::update(cx, |s| s.diff_view = mode);
                Ok(format!("{mode:?}"))
            },
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "toggle diff",
            "toggle diff",
            "Flips the diff view mode between split and unified",
            |_, cx, _| {
                let mode = match settings::get_or_default(cx).diff_view {
                    DiffViewMode::Unified => DiffViewMode::Split,
                    DiffViewMode::Split => DiffViewMode::Unified,
                };
                settings::update(cx, |s| s.diff_view = mode);
                Ok(format!("{mode:?}"))
            },
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "set option",
            "set option <name> <value>",
            "Sets a display setting: changes_as_tree, show_diff_with_log, swap_status_panes, diff_full_file, date_relative (on|off), lines_height (spaced|compact)",
            set_option,
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "key",
            "key <keystroke>...",
            "Dispatches keystrokes inside the app (for example ctrl-f, escape, enter); no OS input",
            |window, cx, args| {
                if args.is_empty() {
                    return Err("missing argument: keystroke".into());
                }
                for word in args.raw() {
                    let keystroke = Keystroke::parse(word)
                        .map_err(|e| format!("bad keystroke '{word}': {e}"))?;
                    window.dispatch_keystroke(keystroke, cx);
                }
                Ok(String::new())
            },
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "type",
            "type <text>",
            "Types text into the focused input as key presses inside the app",
            |window, cx, args| {
                let text = args.rest(0);
                for c in text.chars() {
                    window.dispatch_keystroke(keystroke_for_char(c), cx);
                }
                Ok(format!("{} characters", text.chars().count()))
            },
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "action",
            "action <Name>",
            "Dispatches a registered action (Refresh, Pull, CreateBranch, OpenSettings, ...) to the focused element",
            dispatch_action,
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "begin action",
            "begin action <Name>",
            "Like `action`, but does not wait for the app to settle: for an operation that waits for a prompt \
             (use `wait dialog open` next)",
            |window, cx, args| {
                // Its prompts (credential dialogs) are the scenario's own doing, not an unexpected dialog.
                monitor::expect_prompts(cx, true);
                dispatch_action(window, cx, args)
            },
        )
        .without_settle(),
    );
    register_verb(
        cx,
        Verb::new(
            "close dialog",
            "close dialog",
            "Closes the open dialog",
            |window, cx, _| {
                window.close_dialog(cx);
                monitor::scenario_dialog(cx, false);
                Ok(String::new())
            },
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "close menu",
            "close menu",
            "Closes the open popup menu",
            |_, cx, _| {
                crate::menus::close_menu(cx);
                Ok(String::new())
            },
        ),
    );
}

/// `action` and `begin action`.
fn dispatch_action(window: &mut Window, cx: &mut App, args: &Args) -> Result<String, String> {
    let name = args.text(0, "action name")?;
    let full = if name.contains("::") {
        name
    } else {
        format!("gitcthulhu::{name}")
    };
    let action = cx
        .build_action(&full, None)
        .map_err(|e| format!("unknown action '{full}': {e}"))?;
    window.dispatch_action(action, cx);
    Ok(full)
}

/// `open repository`.
fn open_repository(window: &mut Window, cx: &mut App, args: &Args) -> Result<String, String> {
    let given = args.text(0, "repository path or fixture name")?;
    let mut path = PathBuf::from(&given);
    let bare_name = !given.contains(['/', '\\']);
    if bare_name && !path.exists() {
        path = args.sandbox().join("repos").join(&given);
    }
    session(cx)
        .ok_or_else(|| "no QA session".to_owned())?
        .sandbox
        .check_repository(&path)?;
    if !path.exists() {
        return Err(format!("{} does not exist", path.display()));
    }
    let view = app_view(cx)?;
    view.update(cx, |view, cx| {
        view.open_repository(path.clone(), true, window, cx)
    });
    Ok(path.display().to_string())
}

/// `select tab`.
fn select_tab(window: &mut Window, cx: &mut App, args: &Args) -> Result<String, String> {
    let what = args.rest(0);
    if what.is_empty() {
        return Err("missing argument: tab number or title".into());
    }
    let view = app_view(cx)?;
    let count = view.read(cx).tabs().len();
    let index = match what.parse::<usize>() {
        Ok(n) if (1..=count).contains(&n) => n - 1,
        Ok(n) => return Err(format!("tab {n} does not exist ({count} tabs)")),
        Err(_) => {
            let needle = what.to_lowercase();
            (0..count)
                .find(|&ix| {
                    view.read(cx)
                        .tab_title(ix, cx)
                        .to_lowercase()
                        .contains(&needle)
                })
                .ok_or_else(|| format!("no tab titled '{what}'"))?
        }
    };
    view.update(cx, |view, cx| view.select_tab(index, window, cx));
    Ok(format!("tab {}", index + 1))
}

/// `close tab`.
fn close_tab(window: &mut Window, cx: &mut App, args: &Args) -> Result<String, String> {
    let view = app_view(cx)?;
    let (index, id) = {
        let view = view.read(cx);
        let index = match args.opt(0) {
            Some(n) => n
                .parse::<usize>()
                .ok()
                .filter(|n| (1..=view.tabs().len()).contains(n))
                .map(|n| n - 1)
                .ok_or_else(|| format!("no tab '{n}'"))?,
            None => view.active(),
        };
        let slot = view
            .tabs()
            .get(index)
            .ok_or_else(|| "there is no tab to close".to_owned())?;
        (index, slot.id)
    };
    view.update(cx, |view, cx| view.close_tab(id, window, cx));
    Ok(format!("tab {}", index + 1))
}

/// `set option`.
fn set_option(_: &mut Window, cx: &mut App, args: &Args) -> Result<String, String> {
    let name = args.text(0, "option name")?;
    let value = args.text(1, "value")?.to_lowercase();
    let flag = || match value.as_str() {
        "on" | "true" | "yes" => Ok(true),
        "off" | "false" | "no" => Ok(false),
        other => Err(format!("'{other}' is not on or off")),
    };
    match name.as_str() {
        "changes_as_tree" => {
            let on = flag()?;
            settings::update(cx, |s| s.changes_as_tree = on);
        }
        "show_diff_with_log" => {
            let on = flag()?;
            settings::update(cx, |s| s.show_diff_with_log = on);
        }
        "swap_status_panes" => {
            let on = flag()?;
            settings::update(cx, |s| s.swap_status_panes = on);
        }
        "diff_full_file" => {
            let on = flag()?;
            settings::update(cx, |s| s.diff_full_file = on);
        }
        "date_relative" => {
            let on = flag()?;
            settings::update(cx, |s| s.date_relative = on);
        }
        "lines_height" => {
            let height = match value.as_str() {
                "spaced" => LinesHeight::Spaced,
                "compact" => LinesHeight::Compact,
                other => return Err(format!("'{other}' is not spaced or compact")),
            };
            settings::update(cx, |s| s.lines_height = height);
        }
        other => return Err(format!("unknown option '{other}'")),
    }
    Ok(format!("{name} = {value}"))
}

/// `1280x720` (also `1280 720`) in logical pixels.
fn parse_size(text: &str) -> Result<(f32, f32), String> {
    let (w, h) = text
        .split_once(['x', 'X'])
        .ok_or_else(|| format!("'{text}' is not <width>x<height>"))?;
    let number = |s: &str| {
        s.trim()
            .parse::<f32>()
            .ok()
            .filter(|v| (200.0..=8000.0).contains(v))
            .ok_or_else(|| format!("'{s}' is not a size between 200 and 8000"))
    };
    Ok((number(w)?, number(h)?))
}

/// The keystroke of a typed character.
fn keystroke_for_char(c: char) -> Keystroke {
    let mut keystroke = Keystroke {
        modifiers: Default::default(),
        key: c.to_string(),
        key_char: Some(c.to_string()),
    };
    match c {
        ' ' => keystroke.key = "space".into(),
        '\n' => {
            keystroke.key = "enter".into();
            keystroke.key_char = Some("\n".into());
        }
        '\t' => keystroke.key = "tab".into(),
        c if c.is_uppercase() => {
            keystroke.modifiers.shift = true;
            keystroke.key = c.to_lowercase().collect();
        }
        _ => {}
    }
    keystroke
}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;

    use super::*;

    #[test]
    fn parses_window_sizes() {
        assert_eq!(parse_size("1280x720"), Ok((1280.0, 720.0)), "plain");
        assert_eq!(parse_size("900X600"), Ok((900.0, 600.0)), "capital X");
        assert!(parse_size("1280").is_err(), "no separator");
        assert!(parse_size("10x10").is_err(), "too small to be a window");
    }

    #[test]
    fn typed_characters_become_keystrokes() {
        let space = keystroke_for_char(' ');
        assert_eq!(space.key, "space", "space has a key name");
        let upper = keystroke_for_char('G');
        assert!(
            upper.modifiers.shift && upper.key == "g",
            "capitals are shift + letter"
        );
        assert_eq!(
            keystroke_for_char('a').key_char.as_deref(),
            Some("a"),
            "letters type themselves"
        );
    }
}
