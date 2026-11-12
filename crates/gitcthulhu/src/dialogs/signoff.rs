//! The Sign-off dialog (G10; 09 §3.12): the sign-off format and whether the repository appends it to commit
//! messages. The values live in `repos.json` (`RepoPref::signoff`), not in the Git configuration.

use std::path::PathBuf;

use gpui_kit::component::WindowExt as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _,
    Subscription, Window, div,
};

use super::IconBasedDialog;
use super::register::common::{self, FocusOnce};
use crate::icons::AppIcon;
use crate::metrics::dialogs::SIGNOFF_W;
use crate::metrics::dp;
use crate::settings::Signoff;
use crate::tab::TabCtx;
use crate::widgets::field::{FieldEvent, OutlinedField};
use crate::widgets::{CheckboxText, observed};

/// The format used until the user edits it (`%user` and `%email` are substituted when committing).
pub const DEFAULT_FORMAT: &str = "Signed-off-by: %user <%email>";

/// Element ids of the sign-off dialog.
pub mod ids {
    /// The dialog content.
    pub const SIGNOFF_DIALOG: &str = "signoff-dialog";
    /// The format field.
    pub const SIGNOFF_FORMAT: &str = "signoff-format";
    /// The "Enable sign-off for this repository" checkbox.
    pub const SIGNOFF_ENABLED: &str = "signoff-enabled";
}

/// The stored sign-off of the repository at `path` with the default format filled in.
pub fn stored_signoff(cx: &App, path: &std::path::Path) -> Signoff {
    let mut signoff = cx
        .try_global::<crate::app::AppState>()
        .map(|state| state.repo_prefs.get(path).signoff)
        .unwrap_or_default();
    if signoff.text.trim().is_empty() {
        signoff.text = DEFAULT_FORMAT.to_owned();
    }
    signoff
}

/// The content of an open sign-off dialog.
struct SignoffDialog {
    path: Option<PathBuf>,
    format: Entity<OutlinedField>,
    enabled: bool,
    focus: FocusOnce,
    _subscription: Subscription,
}

impl SignoffDialog {
    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let signoff = Signoff {
            enabled: self.enabled,
            text: common::text_of(&self.format, cx),
        };
        window.close_dialog(cx);
        if let Some(path) = self.path.clone() {
            crate::app::update_repo_prefs(cx, move |prefs| {
                prefs.edit(&path, |pref| pref.signoff = signoff)
            });
        }
    }
}

impl Render for SignoffDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.focus.run(&self.format, false, window, cx);
        let this = cx.weak_entity();
        let toggle = this.clone();
        let content = observed(ids::SIGNOFF_DIALOG)
            .w(dp(SIGNOFF_W))
            .flex()
            .flex_col()
            .child(self.format.clone())
            .child(
                div().pt(dp(8.)).child(
                    CheckboxText::new(
                        ids::SIGNOFF_ENABLED,
                        self.enabled,
                        "Enable sign-off for this repository",
                    )
                    .on_toggle(move |checked, _, cx| {
                        toggle
                            .update(cx, |d, cx| {
                                d.enabled = checked;
                                cx.notify();
                            })
                            .ok();
                    }),
                ),
            );
        IconBasedDialog::new(AppIcon::Sign, "Edit sign-off", "Save")
            .subtitle("Enable or disable the sign-off or adjust its format")
            .without_cancel()
            .content(content)
            .on_primary(move |window, cx| {
                this.update(cx, |d, cx| d.save(window, cx)).ok();
            })
    }
}

/// Opens the Sign-off dialog of the tab's repository.
pub fn open(window: &mut Window, cx: &mut App, tab: TabCtx) {
    let path = tab.backend.handle().and_then(|handle| handle.workdir());
    let stored = path
        .as_deref()
        .map(|path| stored_signoff(cx, path))
        .unwrap_or_else(|| Signoff {
            enabled: false,
            text: DEFAULT_FORMAT.to_owned(),
        });
    let format = common::new_field(ids::SIGNOFF_FORMAT, None, &stored.text, window, cx);
    let view = cx.new(|cx| {
        let subscription = cx.subscribe_in(
            &format,
            window,
            |this: &mut SignoffDialog, _, event: &FieldEvent, window, cx| match event {
                FieldEvent::Changed => cx.notify(),
                FieldEvent::Enter => this.save(window, cx),
            },
        );
        SignoffDialog {
            path,
            format,
            enabled: stored.enabled,
            focus: FocusOnce::default(),
            _subscription: subscription,
        }
    });
    common::open_view(window, cx, SIGNOFF_W, 340., view);
}
