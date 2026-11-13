//! Platform side effects of the toolbar (directory picker, file manager). They panic on GPUI's test platform, so
//! tests route them through the recorder below instead (spec 21 §8.1).

use std::path::{Path, PathBuf};

use gpui_kit::{App, PathPromptOptions, Window};

/// Called with the directory the user picked.
pub type OnPicked = Box<dyn FnOnce(PathBuf, &mut Window, &mut App)>;

#[cfg(test)]
pub use self::recorded::{calls, install_fake};

/// Opens the system's directory picker; `on_picked` runs on the UI thread with the chosen directory (not at all
/// when the picker is cancelled).
pub fn pick_directory(window: &mut Window, cx: &mut App, on_picked: OnPicked) {
    #[cfg(test)]
    if let Some(answer) = recorded::pick(cx) {
        if let Some(path) = answer {
            on_picked(path, window, cx);
        }
        return;
    }
    let paths = cx.prompt_for_paths(PathPromptOptions {
        files: false,
        directories: true,
        multiple: false,
        prompt: None,
    });
    window
        .spawn(cx, async move |cx| {
            let Ok(Ok(Some(paths))) = paths.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            cx.update(|window, cx| on_picked(path, window, cx)).ok();
        })
        .detach();
}

/// Shows `path` in the system's file manager.
pub fn open_in_file_manager(cx: &mut App, path: &Path) {
    #[cfg(test)]
    if recorded::open(cx, path) {
        return;
    }
    cx.open_with_system(path);
}

#[cfg(test)]
mod recorded {
    use std::path::{Path, PathBuf};

    use gpui_kit::{App, Global};

    /// The fake platform of a test: the answer of the directory picker and the recorded calls.
    #[derive(Default)]
    pub struct FakePlatform {
        /// `Some(Some(dir))`: the picker returns `dir`; `Some(None)` or `None`: it is cancelled.
        pub picker: Option<Option<PathBuf>>,
        /// `pick:` and `open:` lines in call order.
        pub calls: Vec<String>,
    }

    impl Global for FakePlatform {}

    /// Replaces the platform by the fake for the rest of the test.
    pub fn install_fake(cx: &mut App, picker: Option<Option<PathBuf>>) {
        cx.set_global(FakePlatform {
            picker,
            calls: Vec::new(),
        });
    }

    /// The calls recorded so far.
    pub fn calls(cx: &App) -> Vec<String> {
        cx.try_global::<FakePlatform>()
            .map(|f| f.calls.clone())
            .unwrap_or_default()
    }

    pub(super) fn pick(cx: &mut App) -> Option<Option<PathBuf>> {
        let fake = cx.try_global::<FakePlatform>()?;
        let answer = fake.picker.clone().flatten();
        cx.global_mut::<FakePlatform>()
            .calls
            .push("pick".to_owned());
        Some(answer)
    }

    pub(super) fn open(cx: &mut App, path: &Path) -> bool {
        if !cx.has_global::<FakePlatform>() {
            return false;
        }
        cx.global_mut::<FakePlatform>()
            .calls
            .push(format!("open:{}", path.display()));
        true
    }
}
