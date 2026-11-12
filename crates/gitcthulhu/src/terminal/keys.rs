//! Keys and mouse buttons for the shell: which keystrokes the terminal takes from the app (`NoAction` bindings),
//! how a key press is routed (text through the input handler, clipboard chords, or the encoder), and the mapping of
//! GPUI key names and modifiers onto libghostty's (ported from the old `but-gpui` view, 18 §4.1).

use but_tty::{Key, Mods, mouse};
use gpui_kit::{KeyBinding, Keystroke, Modifiers, MouseButton, NoAction};

use super::TERMINAL_CONTEXT;

/// The ancestor key the terminal leaves to the app: Ctrl+` toggles the panel, also from inside it.
const TOGGLE_KEY: &str = "ctrl-`";

/// `NoAction` bindings in the `Terminal` context: every app binding of an ancestor context (`ANCESTOR_KEYS`) but
/// the toggle, and Tab / Shift+Tab, so these keys reach the terminal's key handler (gpui-pre `keymap.rs`: a
/// `NoAction` binding in a deeper context suppresses the out-ranked bindings).
pub(super) fn bindings() -> Vec<KeyBinding> {
    crate::actions::ANCESTOR_KEYS
        .iter()
        .map(|(key, _)| *key)
        .filter(|key| *key != TOGGLE_KEY)
        .chain(["tab", "shift-tab"])
        .map(|key| KeyBinding::new(key, NoAction, Some(TERMINAL_CONTEXT)))
        .collect()
}

/// What a key press over the terminal does.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum KeyRoute {
    /// Text the platform delivers through the input handler (typed characters, AltGr characters, dead-key
    /// compositions, IME): nothing to do on the key press itself, or it would arrive twice.
    Text,
    /// Copy the selection.
    Copy,
    /// Paste the clipboard.
    Paste,
    /// Ctrl+C: copy if something is selected, else send the interrupt.
    CopyOrInterrupt,
    /// Select everything (macOS Cmd+A).
    SelectAll,
    /// A chord that stays the app's (macOS Cmd chords): let it propagate.
    App,
    /// Encode the key for the program running, as its terminal modes ask.
    Encode {
        key: Key,
        mods: Mods,
        text: Option<String>,
        unshifted: Option<char>,
    },
}

/// Routes `keystroke`; `prefer_character_input` is set by the platform for AltGr characters and dead keys, whose text
/// comes through the input handler.
pub(super) fn route(keystroke: &Keystroke, prefer_character_input: bool) -> KeyRoute {
    let m = keystroke.modifiers;
    let key = keystroke.key.as_str();
    if cfg!(target_os = "macos") && m.platform {
        let alone = !(m.shift || m.alt || m.control);
        return match key {
            "c" if alone => KeyRoute::Copy,
            "v" if alone => KeyRoute::Paste,
            "a" if alone => KeyRoute::SelectAll,
            _ => KeyRoute::App,
        };
    }
    // Windows Terminal / Ghostty chords off macOS.
    match (key, m.control, m.shift, m.alt) {
        ("c", true, true, false) | ("insert", true, false, false) => return KeyRoute::Copy,
        ("v", true, true, false) | ("insert", false, true, false) => return KeyRoute::Paste,
        ("c", true, false, false) => return KeyRoute::CopyOrInterrupt,
        _ => {}
    }
    let printable = keystroke
        .key_char
        .as_deref()
        .is_some_and(|text| !text.is_empty() && !text.chars().any(char::is_control));
    if printable && (prefer_character_input || !(m.control || m.alt || m.platform)) {
        return KeyRoute::Text;
    }
    let text = keystroke
        .key_char
        .clone()
        .filter(|text| !text.chars().any(char::is_control));
    let mut chars = key.chars();
    let unshifted = match key {
        "space" => Some(' '),
        _ => chars.next().filter(|_| chars.next().is_none()),
    };
    KeyRoute::Encode {
        key: key_for(key),
        mods: mods(m),
        text,
        unshifted,
    }
}

/// GPUI's modifiers as libghostty's.
pub(super) fn mods(modifiers: Modifiers) -> Mods {
    let mut mods = Mods::empty();
    mods.set(Mods::SHIFT, modifiers.shift);
    mods.set(Mods::ALT, modifiers.alt);
    mods.set(Mods::CTRL, modifiers.control);
    mods
}

/// GPUI's mouse button as libghostty's (navigation buttons are not reported).
pub(super) fn button(button: MouseButton) -> Option<mouse::Button> {
    match button {
        MouseButton::Left => Some(mouse::Button::Left),
        MouseButton::Right => Some(mouse::Button::Right),
        MouseButton::Middle => Some(mouse::Button::Middle),
        MouseButton::Navigate(_) => None,
    }
}

/// Whether a drag selects a block: Alt on macOS, Ctrl+Alt elsewhere, as in Ghostty.
pub(super) fn rectangle(modifiers: Modifiers) -> bool {
    modifiers.alt && (cfg!(target_os = "macos") || modifiers.control)
}

/// Whether Cmd alone (Ctrl alone off macOS) is held, for a click to open the link under the pointer.
pub(super) fn follows_links(modifiers: Modifiers) -> bool {
    modifiers == Modifiers::secondary_key()
}

/// The physical key GPUI names `key`, for the encoder to look up what to send.
pub(super) fn key_for(key: &str) -> Key {
    let mut chars = key.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        const LETTERS: [Key; 26] = [
            Key::A,
            Key::B,
            Key::C,
            Key::D,
            Key::E,
            Key::F,
            Key::G,
            Key::H,
            Key::I,
            Key::J,
            Key::K,
            Key::L,
            Key::M,
            Key::N,
            Key::O,
            Key::P,
            Key::Q,
            Key::R,
            Key::S,
            Key::T,
            Key::U,
            Key::V,
            Key::W,
            Key::X,
            Key::Y,
            Key::Z,
        ];
        const DIGITS: [Key; 10] = [
            Key::Digit0,
            Key::Digit1,
            Key::Digit2,
            Key::Digit3,
            Key::Digit4,
            Key::Digit5,
            Key::Digit6,
            Key::Digit7,
            Key::Digit8,
            Key::Digit9,
        ];
        return match c.to_ascii_lowercase() {
            c @ 'a'..='z' => LETTERS
                .get(usize::from(c as u8 - b'a'))
                .copied()
                .unwrap_or(Key::Unidentified),
            c @ '0'..='9' => DIGITS
                .get(usize::from(c as u8 - b'0'))
                .copied()
                .unwrap_or(Key::Unidentified),
            '`' => Key::Backquote,
            '\\' => Key::Backslash,
            '[' => Key::BracketLeft,
            ']' => Key::BracketRight,
            ',' => Key::Comma,
            '=' => Key::Equal,
            '-' => Key::Minus,
            '.' => Key::Period,
            '\'' => Key::Quote,
            ';' => Key::Semicolon,
            '/' => Key::Slash,
            ' ' => Key::Space,
            _ => Key::Unidentified,
        };
    }
    const FUNCTION: [Key; 12] = [
        Key::F1,
        Key::F2,
        Key::F3,
        Key::F4,
        Key::F5,
        Key::F6,
        Key::F7,
        Key::F8,
        Key::F9,
        Key::F10,
        Key::F11,
        Key::F12,
    ];
    match key {
        "enter" => Key::Enter,
        "backspace" => Key::Backspace,
        "tab" => Key::Tab,
        "escape" => Key::Escape,
        "space" => Key::Space,
        "delete" => Key::Delete,
        "insert" => Key::Insert,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "up" => Key::ArrowUp,
        "down" => Key::ArrowDown,
        "left" => Key::ArrowLeft,
        "right" => Key::ArrowRight,
        _ => (key.strip_prefix('f'))
            .and_then(|n| n.parse::<usize>().ok())
            .and_then(|n| FUNCTION.get(n.wrapping_sub(1)).copied())
            .unwrap_or(Key::Unidentified),
    }
}
