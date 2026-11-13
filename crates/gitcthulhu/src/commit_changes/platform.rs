//! Platform side effects of the commit changes region: the clipboard and the file manager. Under `cfg(test)` they
//! only record what was asked (the gpui test platform panics on them).

use std::path::{Path, PathBuf};

use gpui_kit::App;

use crate::backend::{ByteSlice as _, RelaPath};

/// What the tests observed.
#[cfg(test)]
#[derive(Default, Clone, Debug, PartialEq, Eq)]
pub struct Recorded {
    /// Texts written to the clipboard.
    pub clipboard: Vec<String>,
    /// Paths revealed (selected) in the file manager.
    pub revealed: Vec<PathBuf>,
    /// Directories opened in the file manager.
    pub opened: Vec<PathBuf>,
}

#[cfg(test)]
impl gpui_kit::Global for Recorded {}

/// The side effects recorded so far in this test app.
#[cfg(test)]
pub fn recorded(cx: &App) -> Recorded {
    cx.try_global::<Recorded>().cloned().unwrap_or_default()
}

/// Writes `text` to the system clipboard.
pub fn copy_text(cx: &mut App, text: &str) {
    #[cfg(test)]
    {
        cx.default_global::<Recorded>()
            .clipboard
            .push(text.to_owned());
    }
    #[cfg(not(test))]
    {
        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(text.to_owned()));
    }
}

/// The absolute path of the repository-relative `path` below `workdir`.
pub fn absolute_path(workdir: &Path, path: &RelaPath) -> PathBuf {
    workdir.join(path.as_bytes().to_path_lossy())
}

/// What "Open file in folder" shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RevealTarget {
    /// The file exists: select it in the file manager.
    File(PathBuf),
    /// The file is missing: open the nearest existing directory above it.
    Folder(PathBuf),
}

/// Finds what "Open file in folder" opens: the file itself, or, when it does not exist (it was deleted, or the
/// commit is not checked out), the nearest existing directory above it (at most `workdir`). `None` when nothing
/// exists. Touches the file system: call it off the UI thread (`backend::bg`).
pub fn locate(workdir: &Path, path: &RelaPath) -> Option<RevealTarget> {
    let full = absolute_path(workdir, path);
    if full.exists() {
        return Some(RevealTarget::File(full));
    }
    let mut dir = full.parent();
    while let Some(candidate) = dir {
        if candidate.is_dir() {
            return Some(RevealTarget::Folder(candidate.to_owned()));
        }
        if candidate == workdir {
            break;
        }
        dir = candidate.parent();
    }
    None
}

/// Shows `target` in the system file manager.
pub fn open_target(cx: &mut App, target: &RevealTarget) {
    match target {
        RevealTarget::File(path) => reveal(cx, path),
        RevealTarget::Folder(path) => open_dir(cx, path),
    }
}

fn reveal(cx: &mut App, path: &Path) {
    #[cfg(test)]
    {
        cx.default_global::<Recorded>()
            .revealed
            .push(path.to_owned());
    }
    #[cfg(not(test))]
    {
        cx.reveal_path(path);
    }
}

fn open_dir(cx: &mut App, path: &Path) {
    #[cfg(test)]
    {
        cx.default_global::<Recorded>().opened.push(path.to_owned());
    }
    #[cfg(not(test))]
    {
        cx.open_with_system(path);
    }
}
