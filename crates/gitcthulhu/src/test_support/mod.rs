//! The headless test harness (spec 21 §8.1; `#[cfg(test)]`).
//!
//! Rules: every test module starts with `use core::prelude::v1::test;`. GPUI's test dispatcher runs background tasks
//! on the test thread, so nothing in a test may block on a channel that the UI must answer: fixture tabs use
//! `TabOptions { watch: false, prompter: PrompterMode::Refuse }` (`NoPrompter` + `Asker::Auto(false)`; tests that
//! need a "yes" use [`mount_fixture_with`]) and call `reload` explicitly. Kit click helpers dispatch without
//! modifiers and without the middle button: use `input::click_with`. Unit tests never touch the user's directories:
//! `paths` falls back to a per-process temp home under `cfg(test)`.

// The harness serves the region packages; not every helper is used by this crate's own tests yet.
#![allow(dead_code)]

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Entity, Global, Pixels, Size, TestAppContext,
    WeakEntity, px, size,
};
use parking_lot::Mutex;

use crate::app_view::AppView;
use crate::backend::handle::{self, TabBackend};
use crate::backend::present::OpFailure;
use crate::backend::prompt::{Asker, UiRequest};
use crate::backend::{GitStatus, RefreshSet};
use crate::tab::ops::{OpKind, RestoreKind, Scripted};
use crate::tab::{PrompterMode, RepoModel, RepoTab, RepoUi, TabCtx, TabEvents, TabId, TabOptions};

pub mod fixture;
pub mod geometry;
pub mod input;
pub mod seed;

pub use fixture::Fixture;
pub use seed::RepoSeed;

/// The default test window: 1280×720.
pub const DEFAULT_SIZE: Size<Pixels> = Size {
    width: px(1280.),
    height: px(720.),
};

/// What the inert backend recorded.
#[derive(Clone, Debug, PartialEq)]
pub enum TestCall {
    /// An operation (`run_op`), with its oplog title.
    Op { kind: OpKind, title: Option<String> },
    /// A restore (`run_restore`).
    Restore(RestoreKind),
    /// A reload.
    Reload(RefreshSet),
    /// An ahead/behind request for this many pairs.
    AheadBehind(usize),
}

/// The backend of seeded tabs: records operations and reloads; scripted outcomes complete operations.
#[derive(Default)]
pub struct InertBackend {
    calls: Mutex<Vec<TestCall>>,
    scripted: Mutex<VecDeque<Scripted>>,
}

impl InertBackend {
    /// Records a call.
    pub fn record(&self, call: TestCall) {
        self.calls.lock().push(call);
    }

    /// The recorded calls.
    pub fn calls(&self) -> Vec<TestCall> {
        self.calls.lock().clone()
    }

    /// Forgets the recorded calls.
    pub fn clear(&self) {
        self.calls.lock().clear();
    }

    /// The next scripted outcome, if any.
    pub fn next_scripted(&self) -> Option<Scripted> {
        self.scripted.lock().pop_front()
    }

    /// Queues the outcome of the next operation (`V` = `OpOutcome<T>` for `run_op`, `RestoreDone` for
    /// `run_restore`), delivered after `delay` of test time.
    pub fn script<V: Send + 'static>(&self, result: Result<V, OpFailure>, delay: Duration) {
        self.scripted.lock().push_back(Scripted {
            delay,
            result: Box::new(result),
        });
    }
}

/// The receiver of the request pump between `init_app` and `open_app`.
struct PendingRequests(Option<futures::channel::mpsc::UnboundedReceiver<UiRequest>>);

impl Global for PendingRequests {}

/// The kit, the fonts, default settings and UI state, the Dark theme, the actions and registry, `AppState` (Git
/// `Starting`).
pub fn init_app(cx: &mut TestAppContext) {
    geometry::init_visual(cx);
    cx.update(|cx| {
        crate::settings::state::init(cx, crate::settings::UiState::default());
        crate::actions::init(cx);
        let receiver = crate::app::install_state(cx, crate::settings::RepoPrefs::default());
        cx.set_global(PendingRequests(Some(receiver)));
    });
}

/// Opens the main window (AppView with one welcome tab).
pub fn open_app(cx: &mut TestAppContext, size: Size<Pixels>) -> (AnyWindowHandle, Entity<AppView>) {
    cx.update(|cx| {
        let receiver = cx
            .has_global::<PendingRequests>()
            .then(|| cx.global_mut::<PendingRequests>().0.take())
            .flatten()
            .unwrap_or_else(|| {
                crate::app::install_state(cx, crate::settings::RepoPrefs::default())
            });
        gpui_kit::open_window(
            geometry::window_options(size.width.as_f32(), size.height.as_f32()),
            cx,
            move |window, cx| cx.new(|cx| AppView::new(receiver, None, window, cx)),
        )
        .expect("open the test window")
    })
}

/// A seeded tab on the inert backend (operations and reloads recorded; reads never reach a backend), selected in
/// a new app window. Nothing is recorded yet when this returns.
pub fn mount_seeded(
    cx: &mut TestAppContext,
    seed: RepoSeed,
    size: Size<Pixels>,
) -> (AnyWindowHandle, Entity<AppView>, Entity<RepoTab>) {
    init_app(cx);
    let (handle, app) = open_app(cx, size);
    let inert = Arc::new(InertBackend::default());
    let name = seed.name.clone();
    let tab = cx
        .update_window(handle, |_, window, cx| {
            let tab = cx.new(|cx| {
                RepoTab::new(
                    TabId::next(),
                    TabBackend::Inert(inert),
                    name.clone().into(),
                    std::path::PathBuf::from(name),
                    TabOptions {
                        watch: false,
                        prompter: PrompterMode::Refuse,
                    },
                    window,
                    cx,
                )
            });
            app.update(cx, |app, cx| {
                app.add_repo_tab(tab.clone(), true, window, cx)
            });
            tab
        })
        .expect("the test window is open");
    let model = cx.update(|cx| tab.read(cx).ctx().model.clone());
    seed.apply(&model, cx);
    cx.run_until_parked();
    (handle, app, tab)
}

/// A tab over the fixture's real repository (`watch: false`, `prompter: Refuse`), loaded (`reload(ALL)`).
pub fn mount_fixture(
    cx: &mut TestAppContext,
    fixture: &Fixture,
    size: Size<Pixels>,
) -> (AnyWindowHandle, Entity<AppView>, Entity<RepoTab>) {
    mount_fixture_with(cx, fixture, size, false)
}

/// [`mount_fixture`] with `Asker::Auto(asker_answer)` for `prepare` confirmations.
pub fn mount_fixture_with(
    cx: &mut TestAppContext,
    fixture: &Fixture,
    size: Size<Pixels>,
    asker_answer: bool,
) -> (AnyWindowHandle, Entity<AppView>, Entity<RepoTab>) {
    init_app(cx);
    let rt = fixture.runtime();
    cx.update(|cx| {
        if let Some(state) = crate::app::state_mut(cx) {
            state.git = GitStatus::Ready(rt.clone());
        }
    });
    let undo = cx.update(|cx| crate::app::undo_settings(cx));
    let repo_handle =
        handle::open_repo(&rt, fixture.path(), &undo).expect("the fixture repository opens");
    let (window_handle, app) = open_app(cx, size);
    let tab = cx
        .update_window(window_handle, |_, window, cx| {
            let name = repo_handle.display_name();
            let path = repo_handle.workdir().unwrap_or_default();
            let tab = cx.new(|cx| {
                let mut tab = RepoTab::new(
                    TabId::next(),
                    TabBackend::Real(Arc::new(repo_handle)),
                    name,
                    path,
                    TabOptions {
                        watch: false,
                        prompter: PrompterMode::Refuse,
                    },
                    window,
                    cx,
                );
                tab.set_asker(Asker::Auto(asker_answer));
                tab
            });
            app.update(cx, |app, cx| {
                app.add_repo_tab(tab.clone(), true, window, cx)
            });
            tab.update(cx, |tab, cx| tab.reload(RefreshSet::ALL, cx));
            tab
        })
        .expect("the test window is open");
    cx.run_until_parked();
    (window_handle, app, tab)
}

/// The inert backend of a seeded tab.
pub fn inert(tab: &Entity<RepoTab>, cx: &App) -> Arc<InertBackend> {
    match &tab.read(cx).ctx().backend {
        TabBackend::Inert(inert) => inert.clone(),
        TabBackend::Real(_) => panic!("not a seeded tab"),
    }
}

/// The calls the inert backend of a seeded tab recorded.
pub fn calls(tab: &Entity<RepoTab>, cx: &App) -> Vec<TestCall> {
    inert(tab, cx).calls()
}

/// Queues the outcome of the next operation of a seeded tab (see [`InertBackend::script`]).
pub fn script_op<V: Send + 'static>(
    tab: &Entity<RepoTab>,
    cx: &App,
    result: Result<V, OpFailure>,
    delay: Duration,
) {
    inert(tab, cx).script(result, delay);
}

/// A `TabCtx` with fresh entities and no tab (menu and widget tests).
pub fn inert_tab_ctx(cx: &mut App) -> TabCtx {
    TabCtx {
        id: TabId::next(),
        backend: TabBackend::Inert(Arc::new(InertBackend::default())),
        model: cx.new(|_| RepoModel::default()),
        ui: cx.new(|_| RepoUi::new()),
        events: cx.new(|_| TabEvents),
        tab: WeakEntity::new_invalid(),
    }
}

/// The texts of the toasts visible now.
pub fn toast_texts(app: &Entity<AppView>, cx: &App) -> Vec<String> {
    app.read(cx)
        .toasts()
        .read(cx)
        .visible()
        .map(|t| t.text.to_string())
        .collect()
}

/// A window size in logical pixels.
pub fn window_size(w: f32, h: f32) -> Size<Pixels> {
    size(px(w), px(h))
}
