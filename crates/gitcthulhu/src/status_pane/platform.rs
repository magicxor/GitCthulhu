//! Platform side effects of the file menus (clipboard, "Open file in folder"). They panic on the headless test
//! platform, so they go through an injectable function table that tests replace and record.

use std::path::{Path, PathBuf};

use gpui_kit::{App, ClipboardItem, Global};

/// The functions the file menus call for platform side effects.
#[derive(Clone, Copy)]
pub struct FilesPlatform {
    /// Puts text on the clipboard.
    pub copy: fn(&mut App, String),
    /// Shows a path in the platform's file manager.
    pub reveal: fn(&mut App, PathBuf),
}

impl Global for FilesPlatform {}

fn real_copy(cx: &mut App, text: String) {
    cx.write_to_clipboard(ClipboardItem::new_string(text));
}

/// The nearest existing ancestor of `path` (the path itself when it exists): a deleted file is revealed by its
/// folder.
fn existing_ancestor(path: &Path) -> PathBuf {
    let mut current = path.to_path_buf();
    while !current.exists() {
        if !current.pop() {
            break;
        }
    }
    current
}

fn real_reveal(cx: &mut App, path: PathBuf) {
    cx.reveal_path(&existing_ancestor(&path));
}

fn platform(cx: &App) -> FilesPlatform {
    cx.try_global::<FilesPlatform>()
        .copied()
        .unwrap_or(FilesPlatform {
            copy: real_copy,
            reveal: real_reveal,
        })
}

/// Copies `text` to the clipboard.
pub fn copy_text(cx: &mut App, text: String) {
    let copy = platform(cx).copy;
    copy(cx, text);
}

/// Shows `path` in the platform's file manager; for a path that no longer exists, its folder.
pub fn reveal_path(cx: &mut App, path: PathBuf) {
    let reveal = platform(cx).reveal;
    reveal(cx, path);
}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;

    use super::*;

    #[test]
    fn a_missing_file_is_revealed_by_its_folder() {
        let dir = std::env::temp_dir();
        let missing = dir.join("gitcthulhu-g5-missing-dir").join("gone.txt");
        assert_eq!(existing_ancestor(&missing), dir, "walks up to what exists");
        assert_eq!(existing_ancestor(&dir), dir, "an existing path stays");
    }
}
