//! The Clone dialog (G10; 09 §3.1): URL, directory (prefilled with the default clone directory of the settings),
//! folder name, "Save as default", "Clone submodules recursively", an inline error banner, and the cloning state
//! with progress and Cancel. It clones through `AppView::clone_repository`, which opens the clone in a new tab.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use futures::StreamExt as _;
use gpui_kit::component::WindowExt as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Subscription, Task, Window, div,
};

use super::register::common::{self, FocusOnce};
use super::{LayerId, close_layer_or_top};
use crate::backend::prompt::progress_text;
use crate::backend::{CancelToken, CloneOptions, GitErrorKind, validate};
use crate::icons::AppIcon;
use crate::metrics::dialogs::{CLONE_FORM_PAD_H, CLONE_PICK_BUTTON, CLONE_W};
use crate::metrics::dp;
use crate::theme::{ActiveTokens as _, TextStyleExt as _};
use crate::widgets::field::{FieldEvent, OutlinedField};
use crate::widgets::{CheckboxText, CircularProgress, IconButton, PrimaryButton, observed};

/// End offset of the directory picker's 48 dp hit box: its centre is 4 + 40 / 2 = 24 dp from the field's end.
const CLONE_PICK_END: f32 = 4. + CLONE_PICK_BUTTON / 2. - 24.;

/// Progress lines are shown at most this often.
const PROGRESS_THROTTLE: Duration = Duration::from_millis(50);

/// Element ids of the clone dialog.
pub mod ids {
    /// The dialog content.
    pub const CLONE_DIALOG: &str = "clone-dialog";
    /// The URL field.
    pub const CLONE_URL: &str = "clone-url";
    /// The directory field.
    pub const CLONE_DIRECTORY: &str = "clone-directory";
    /// The folder field.
    pub const CLONE_FOLDER: &str = "clone-folder";
    /// The directory picker button.
    pub const CLONE_PICK: &str = "clone-pick";
    /// "Save as default".
    pub const CLONE_SAVE_DEFAULT: &str = "clone-save-default";
    /// "Clone submodules recursively".
    pub const CLONE_RECURSIVE: &str = "clone-recursive";
    /// The error banner.
    pub const CLONE_ERROR: &str = "clone-error";
    /// The Clone button.
    pub const CLONE_START: &str = "clone-start";
    /// The Cancel button of the cloning state.
    pub const CLONE_CANCEL: &str = "clone-cancel";
    /// The progress line of the cloning state.
    pub const CLONE_PROGRESS: &str = "clone-progress";
}

/// What the dialog shows.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Phase {
    Form,
    /// Cloning; the text is the newest progress line.
    Cloning(SharedString),
    /// Cancel was pressed; waiting for the clone to stop.
    Cancelling,
}

/// The content of an open clone dialog.
pub struct CloneDialog {
    url: Entity<OutlinedField>,
    directory: Entity<OutlinedField>,
    folder: Entity<OutlinedField>,
    save_default: bool,
    recursive: bool,
    /// The user typed in the folder field: it no longer follows the URL.
    folder_touched: bool,
    error: Option<SharedString>,
    phase: Phase,
    cancel: Option<CancelToken>,
    last_progress: Option<Instant>,
    focus: FocusOnce,
    /// The layer of this dialog: it closes by id, so that another dialog opened above it stays open.
    layer: Option<LayerId>,
    _subscriptions: Vec<Subscription>,
    _progress: Option<Task<()>>,
}

impl Drop for CloneDialog {
    fn drop(&mut self) {
        // Escape (or any other way of closing the dialog) while cloning cancels the clone.
        if let Some(cancel) = &self.cancel {
            cancel.cancel();
        }
    }
}

impl CloneDialog {
    fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let url = common::text_of(&self.url, cx);
        let directory = common::text_of(&self.directory, cx);
        let folder = common::text_of(&self.folder, cx);
        if let Some(error) = validate::clone_form_error(&url, &directory, &folder) {
            self.error = Some(error.into());
            cx.notify();
            return;
        }
        let Some(app) = cx
            .try_global::<crate::app_view::AppViewGlobal>()
            .and_then(|global| global.0.upgrade())
        else {
            self.error = Some("The window is not available".into());
            cx.notify();
            return;
        };
        let target = PathBuf::from(directory.trim()).join(folder.trim());
        let options = CloneOptions {
            url: url.trim().to_owned(),
            directory: target,
            branch: None,
            depth: None,
            recurse_submodules: self.recursive,
            bare: false,
        };
        self.error = None;
        self.phase = Phase::Cloning("Cloning...".into());
        let this = cx.weak_entity();
        let save_default = self.save_default.then(|| PathBuf::from(directory.trim()));
        let job = app.update(cx, |app, cx| {
            app.clone_repository(
                options,
                Box::new(move |result, window, cx| {
                    this.update(cx, |dialog, cx| {
                        dialog.finished(result, save_default, window, cx)
                    })
                    .ok();
                }),
                window,
                cx,
            )
        });
        self.cancel = Some(job.cancel);
        let mut progress = job.progress;
        self._progress = Some(cx.spawn_in(window, async move |this, cx| {
            while let Some(event) = progress.next().await {
                let Some(text) = progress_text(&event) else {
                    continue;
                };
                let alive = this.update(cx, |dialog, cx| dialog.show_progress(text, cx));
                if alive.is_err() {
                    break;
                }
            }
        }));
        cx.notify();
    }

    fn show_progress(&mut self, text: SharedString, cx: &mut Context<Self>) {
        let Phase::Cloning(line) = &mut self.phase else {
            return;
        };
        // Always keep the newest line; only the repaint is throttled, so the next frame never shows a stale one.
        *line = text;
        let due = self
            .last_progress
            .is_none_or(|at| at.elapsed() >= PROGRESS_THROTTLE);
        if due {
            self.last_progress = Some(Instant::now());
            cx.notify();
        }
    }

    /// The clone ended (`AppView` already opened the tab on success).
    fn finished(
        &mut self,
        result: Result<(), std::sync::Arc<crate::backend::GitError>>,
        save_default: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cancel = None;
        match result {
            Ok(()) => {
                if let Some(directory) = save_default {
                    crate::settings::update(cx, move |settings| {
                        settings.clone_default_directory = Some(directory)
                    });
                }
                close_layer_or_top(window, cx, self.layer);
            }
            Err(err) => {
                self.phase = Phase::Form;
                if !matches!(
                    err.kind(),
                    GitErrorKind::Cancelled | GitErrorKind::AuthCancelled
                ) {
                    self.error = Some(err.to_string().into());
                }
                cx.notify();
            }
        }
    }

    fn cancel_clone(&mut self, cx: &mut Context<Self>) {
        if let Some(cancel) = &self.cancel {
            cancel.cancel();
            self.phase = Phase::Cancelling;
            cx.notify();
        }
    }

    fn form(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let tokens = *cx.tokens();
        self.focus.run(&self.url, false, window, cx);
        let this = cx.weak_entity();
        let save = this.clone();
        let recursive = this.clone();
        let pick = this.clone();
        let start = this;
        let picker_target = self.directory.clone();
        div()
            .w_full()
            .px(dp(CLONE_FORM_PAD_H))
            .flex()
            .flex_col()
            .child(
                div()
                    .py(dp(4.))
                    .text_h4()
                    .semibold()
                    .child("Clone a new repository"),
            )
            .child(
                div()
                    .pt(dp(8.))
                    .pb(dp(8.))
                    .child(common::form_label("URL", &tokens)),
            )
            .child(self.url.clone())
            .child(
                div()
                    .pt(dp(16.))
                    .pb(dp(8.))
                    .child(common::form_label("Directory", &tokens)),
            )
            .child(
                div()
                    .relative()
                    .w_full()
                    .child(self.directory.clone())
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            // Gitnuro's 40 dp button sits 4 dp from the end; the kit's 48 dp hit box is
                            // centred on the same point (24 dp from the field's end).
                            .right(dp(CLONE_PICK_END))
                            .flex()
                            .items_center()
                            .child(
                                IconButton::new(ids::CLONE_PICK, AppIcon::Search)
                                    .tooltip("Search")
                                    .on_click(move |window, cx| {
                                        let target = picker_target.clone();
                                        let pick = pick.clone();
                                        common::pick_folder(window, cx, move |path, window, cx| {
                                            let text = path.to_string_lossy().into_owned();
                                            target
                                                .update(cx, |f, cx| f.set_text(&text, window, cx));
                                            // A new directory is an edit: it clears the error (09 §3.1).
                                            pick.update(cx, |d, cx| {
                                                d.error = None;
                                                cx.notify();
                                            })
                                            .ok();
                                        });
                                    }),
                            ),
                    ),
            )
            .child(
                CheckboxText::new(
                    ids::CLONE_SAVE_DEFAULT,
                    self.save_default,
                    "Save as default",
                )
                .on_toggle(move |checked, _, cx| {
                    save.update(cx, |d, cx| {
                        d.save_default = checked;
                        cx.notify();
                    })
                    .ok();
                }),
            )
            .child(
                div()
                    .pt(dp(16.))
                    .pb(dp(8.))
                    .child(common::form_label("Folder", &tokens)),
            )
            .child(self.folder.clone())
            .child(
                div().pt(dp(16.)).child(
                    CheckboxText::new(
                        ids::CLONE_RECURSIVE,
                        self.recursive,
                        "Clone submodules recursively",
                    )
                    .body2()
                    .on_toggle(move |checked, _, cx| {
                        recursive
                            .update(cx, |d, cx| {
                                d.recursive = checked;
                                cx.notify();
                            })
                            .ok();
                    }),
                ),
            )
            .when_some(self.error.clone(), |col, error| {
                col.child(observed(ids::CLONE_ERROR).child(common::error_banner(error, &tokens)))
            })
            .child(
                div()
                    .pt(dp(16.))
                    .flex()
                    .flex_row()
                    .justify_end()
                    .child(
                        div().pr(dp(8.)).child(
                            PrimaryButton::new("dialog-cancel", "Cancel")
                                .cancel()
                                .on_click(|window, cx| window.close_dialog(cx)),
                        ),
                    )
                    .child(PrimaryButton::new(ids::CLONE_START, "Clone").on_click(
                        move |window, cx| {
                            start.update(cx, |d, cx| d.start(window, cx)).ok();
                        },
                    )),
            )
            .into_any_element()
    }
}

impl Render for CloneDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = *cx.tokens();
        let body = match self.phase.clone() {
            Phase::Form => self.form(window, cx),
            Phase::Cloning(line) => {
                let this = cx.weak_entity();
                div()
                    .w_full()
                    .relative()
                    .flex()
                    .flex_col()
                    .items_center()
                    .child(observed(ids::CLONE_PROGRESS).text_body1().child(line))
                    .child(div().py(dp(16.)).child(CircularProgress::new()))
                    .child(
                        div()
                            .w_full()
                            .flex()
                            .flex_row()
                            .justify_end()
                            .pr(dp(8.))
                            // Gitnuro `TextButton("Cancel")`: on_background body 1 text, no fill.
                            .child(
                                PrimaryButton::new(ids::CLONE_CANCEL, "Cancel")
                                    .cancel()
                                    .on_click(move |_, cx| {
                                        this.update(cx, |d, cx| d.cancel_clone(cx)).ok();
                                    }),
                            ),
                    )
                    .into_any_element()
            }
            Phase::Cancelling => div()
                .w_full()
                .flex()
                .flex_col()
                .items_center()
                .child(div().px(dp(16.)).child(CircularProgress::new()))
                .child(
                    div()
                        .py(dp(16.))
                        .text_body1()
                        .text_color(tokens.on_background)
                        .child("Cancelling clone operation..."),
                )
                .into_any_element(),
        };
        observed(ids::CLONE_DIALOG).w(dp(CLONE_W)).child(body)
    }
}

/// Opens the Clone dialog.
pub fn open(window: &mut Window, cx: &mut App) {
    let default_directory = crate::settings::get_or_default(cx)
        .clone_default_directory
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default();
    let url = common::new_field(ids::CLONE_URL, None, "", window, cx);
    let directory = cx.new(|cx| {
        let mut field =
            OutlinedField::new(ids::CLONE_DIRECTORY, window, cx).trailing_space(CLONE_PICK_BUTTON);
        field.set_text(&default_directory, window, cx);
        field
    });
    let folder = common::new_field(ids::CLONE_FOLDER, None, "", window, cx);
    let view = cx.new(|cx| {
        let mut subscriptions = Vec::new();
        subscriptions.push(cx.subscribe_in(
            &url,
            window,
            |this: &mut CloneDialog, _, event: &FieldEvent, window, cx| {
                if *event != FieldEvent::Changed {
                    return;
                }
                this.error = None;
                if !this.folder_touched {
                    let name = validate::repo_name_from_url(&common::text_of(&this.url, cx));
                    this.folder
                        .update(cx, |field, cx| field.set_text(&name, window, cx));
                }
                cx.notify();
            },
        ));
        subscriptions.push(cx.subscribe(
            &directory,
            |this: &mut CloneDialog, _, event: &FieldEvent, cx| {
                if *event == FieldEvent::Changed {
                    this.error = None;
                    cx.notify();
                }
            },
        ));
        subscriptions.push(cx.subscribe(
            &folder,
            |this: &mut CloneDialog, _, event: &FieldEvent, cx| {
                if *event == FieldEvent::Changed {
                    this.folder_touched = true;
                    this.error = None;
                    cx.notify();
                }
            },
        ));
        CloneDialog {
            url,
            directory,
            folder,
            save_default: false,
            recursive: true,
            folder_touched: false,
            error: None,
            phase: Phase::Form,
            cancel: None,
            last_progress: None,
            focus: FocusOnce::default(),
            layer: None,
            _subscriptions: subscriptions,
            _progress: None,
        }
    });
    let layer = common::open_view(window, cx, CLONE_W, 560., view.clone());
    view.update(cx, |this, _| this.layer = Some(layer));
}
