//! Platform side effects of the shell behind an injectable seam: the native directory picker and URL launches.
//!
//! They panic on the headless test platform, so the shell calls them through this module and tests install
//! recording replacements with [`install`].

use std::path::PathBuf;
use std::rc::Rc;

use gpui_kit::{App, Global, PathPromptOptions, Window};

/// Called with the directory the user picked (never called when the picker was cancelled).
pub type OnPicked = Box<dyn FnOnce(PathBuf, &mut Window, &mut App)>;

type PickDirectory = Rc<dyn Fn(&mut Window, &mut App, OnPicked)>;
type OpenUrl = Rc<dyn Fn(&mut App, &str)>;

/// The platform services of the shell.
#[derive(Clone)]
pub struct ShellPlatform {
    pick_directory: PickDirectory,
    open_url: OpenUrl,
}

impl ShellPlatform {
    /// A platform with custom services (tests).
    pub fn new(
        pick_directory: impl Fn(&mut Window, &mut App, OnPicked) + 'static,
        open_url: impl Fn(&mut App, &str) + 'static,
    ) -> Self {
        Self {
            pick_directory: Rc::new(pick_directory),
            open_url: Rc::new(open_url),
        }
    }

    /// The real services: the native directory dialog and the default browser.
    pub fn native() -> Self {
        Self::new(native_pick_directory, |cx, url| cx.open_url(url))
    }
}

struct ShellPlatformGlobal(ShellPlatform);

impl Global for ShellPlatformGlobal {}

/// Replaces the platform services of the shell.
pub fn install(cx: &mut App, platform: ShellPlatform) {
    cx.set_global(ShellPlatformGlobal(platform));
}

fn current(cx: &App) -> ShellPlatform {
    cx.try_global::<ShellPlatformGlobal>()
        .map(|g| g.0.clone())
        .unwrap_or_else(ShellPlatform::native)
}

/// Asks the user for a directory; `done` runs on the UI thread with the choice.
pub fn pick_directory(window: &mut Window, cx: &mut App, done: OnPicked) {
    let service = current(cx).pick_directory;
    service(window, cx, done);
}

/// Opens `url` in the default browser.
pub fn open_url(cx: &mut App, url: &str) {
    let service = current(cx).open_url;
    service(cx, url);
}

fn native_pick_directory(window: &mut Window, cx: &mut App, done: OnPicked) {
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
            cx.update(|window, cx| done(path, window, cx)).ok();
        })
        .detach();
}
