//! The Settings dialog (owned by G11; 09 §3.18, spec 21 §5.4): a 1000×720 frame with the category list on the
//! left and the setting rows of the selected category on the right. Every change is applied immediately; "Accept"
//! only closes the dialog.
//!
//! The INTEGRATIONS entries, the Updates entry and the embedded-terminal rows appear only when their
//! `*_available(cx)` slots return true (milestone 1: never). Changing the Git executable, the proxy or the
//! security settings rebuilds the Git runtime for repositories opened afterwards ([`register`]).

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::base::POPUP_PRIORITY;
use gpui_kit::component::WindowExt as _;
use gpui_kit::component::dialog::DialogEntrance;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, BoxShadow, Context, ElementId, Entity, Global,
    InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, ParentElement as _, Pixels,
    Render, RenderOnce, SharedString, Size, StatefulInteractiveElement as _, Styled as _,
    Subscription, Task, TestSupportExt as _, WeakEntity, Window, anchored, deferred, div, point,
    px, transparent_black,
};

use crate::icons::{AppIcon, icon};
use crate::metrics::{self, dp, dpx, settings as sm};
use crate::settings::{self, SettingsGlobal};
use crate::theme::{ActiveTokens as _, TextStyleExt as _};
use crate::widgets::field::{FieldEvent, OutlinedField};
use crate::widgets::scroll::scroll_y;
use crate::widgets::{BoundsCell, PrimaryButton, observed, track_bounds, with_layer};

pub mod accounts;
pub mod ai;
mod inputs;
pub mod pages;
mod runtime;
pub mod terminal;
pub mod updates;

use inputs::{NumberEvent, NumberField, ThemeEditor};
pub use pages::{date_preview, valid_date_pattern};
pub use runtime::{
    RuntimeKey, SecretStore, SecretStoreGlobal, rebuild_now, register_runtime_watch,
};

/// Element ids of the dialog (tests; spec 21 §8.2).
pub mod ids {
    use gpui_kit::SharedString;

    use super::SettingsPage;

    /// The dialog frame (1000 × 720 dp).
    pub const DIALOG: &str = "settings-dialog";
    /// The category column (200 dp).
    pub const LEFT: &str = "settings-left";
    /// The content column.
    pub const CONTENT: &str = "settings-content";
    /// The "Accept" button.
    pub const ACCEPT: &str = "settings-accept";
    /// The custom theme editor.
    pub const THEME_EDITOR: &str = "settings-field-custom-theme";

    /// The category entry of `page`.
    pub fn entry(page: SettingsPage) -> SharedString {
        format!("settings-entry-{}", page.key()).into()
    }

    /// The setting row `key`.
    pub fn row(key: &str) -> SharedString {
        format!("settings-row-{key}").into()
    }

    /// The title column (title, subtitle, notes) of the row `key`.
    pub fn titles(key: &str) -> SharedString {
        format!("settings-titles-{key}").into()
    }

    /// The switch of the row `key`.
    pub fn switch(key: &str) -> SharedString {
        format!("settings-switch-{key}").into()
    }

    /// The trigger of the dropdown `key`.
    pub fn select(key: &str) -> SharedString {
        format!("settings-select-{key}").into()
    }

    /// The popup card of the open dropdown `key`.
    pub fn select_popup(key: &str) -> SharedString {
        format!("settings-select-{key}-popup").into()
    }

    /// Option `index` of the open dropdown `key`.
    pub fn select_option(key: &str, index: usize) -> SharedString {
        format!("settings-select-{key}-option-{index}").into()
    }

    /// The text or number field of the row `key`.
    pub fn field(key: &str) -> SharedString {
        format!("settings-field-{key}").into()
    }
}

/// A category of the Settings dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SettingsPage {
    Appearance,
    Layout,
    DateTime,
    Environment,
    Branches,
    RemoteActions,
    Proxy,
    Authentication,
    Security,
    Accounts,
    Ai,
    Terminal,
    Updates,
    Logs,
}

/// A group of categories in the left column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    UserInterface,
    Git,
    Network,
    Integrations,
    Tools,
}

impl Section {
    /// The header text (shown upper-cased).
    pub fn title(self) -> &'static str {
        match self {
            Section::UserInterface => "User interface",
            Section::Git => "Git",
            Section::Network => "Network",
            Section::Integrations => "Integrations",
            Section::Tools => "Tools",
        }
    }
}

impl SettingsPage {
    /// The entry text.
    pub fn title(self) -> &'static str {
        match self {
            SettingsPage::Appearance => "Appearance",
            SettingsPage::Layout => "Layout",
            SettingsPage::DateTime => "Date/Time",
            SettingsPage::Environment => "Environment",
            SettingsPage::Branches => "Branches",
            SettingsPage::RemoteActions => "Remote actions",
            SettingsPage::Proxy => "Proxy",
            SettingsPage::Authentication => "Authentication",
            SettingsPage::Security => "Security",
            SettingsPage::Accounts => "Accounts",
            SettingsPage::Ai => "AI",
            SettingsPage::Terminal => "Terminal",
            SettingsPage::Updates => "Updates",
            SettingsPage::Logs => "Logs",
        }
    }

    /// A stable lower-case key (element ids).
    pub fn key(self) -> &'static str {
        match self {
            SettingsPage::Appearance => "appearance",
            SettingsPage::Layout => "layout",
            SettingsPage::DateTime => "date-time",
            SettingsPage::Environment => "environment",
            SettingsPage::Branches => "branches",
            SettingsPage::RemoteActions => "remote-actions",
            SettingsPage::Proxy => "proxy",
            SettingsPage::Authentication => "authentication",
            SettingsPage::Security => "security",
            SettingsPage::Accounts => "accounts",
            SettingsPage::Ai => "ai",
            SettingsPage::Terminal => "terminal",
            SettingsPage::Updates => "updates",
            SettingsPage::Logs => "logs",
        }
    }

    /// The entry icon.
    pub fn icon(self) -> AppIcon {
        match self {
            SettingsPage::Appearance => AppIcon::Palette,
            SettingsPage::Layout => AppIcon::Layout,
            SettingsPage::DateTime => AppIcon::Schedule,
            SettingsPage::Environment => AppIcon::Folder,
            SettingsPage::Branches => AppIcon::Branch,
            SettingsPage::RemoteActions => AppIcon::Cloud,
            SettingsPage::Proxy => AppIcon::Network,
            SettingsPage::Authentication => AppIcon::Password,
            SettingsPage::Security => AppIcon::Security,
            SettingsPage::Accounts => AppIcon::Accounts,
            SettingsPage::Ai => AppIcon::Sparkles,
            SettingsPage::Terminal => AppIcon::Terminal,
            SettingsPage::Updates => AppIcon::Updates,
            SettingsPage::Logs => AppIcon::Info,
        }
    }

    /// Whether the content column scrolls (the milestone-2 pages are taller than the dialog).
    fn scrolls(self) -> bool {
        matches!(
            self,
            SettingsPage::Accounts | SettingsPage::Ai | SettingsPage::Updates
        )
    }
}

/// The sections and entries of the left column. Milestone 1 is exactly Gitnuro's list; the milestone-2 entries
/// appear when their `*_available(cx)` slot returns true.
pub fn categories(cx: &App) -> Vec<(Section, Vec<SettingsPage>)> {
    let mut integrations = Vec::new();
    if crate::forge::accounts::available(cx) {
        integrations.push(SettingsPage::Accounts);
    }
    if crate::ai::settings_available(cx) {
        integrations.push(SettingsPage::Ai);
    }
    let mut tools = vec![SettingsPage::Terminal];
    if crate::update::settings_available(cx) {
        tools.push(SettingsPage::Updates);
    }
    tools.push(SettingsPage::Logs);
    let mut sections = vec![
        (
            Section::UserInterface,
            vec![
                SettingsPage::Appearance,
                SettingsPage::Layout,
                SettingsPage::DateTime,
            ],
        ),
        (
            Section::Git,
            vec![
                SettingsPage::Environment,
                SettingsPage::Branches,
                SettingsPage::RemoteActions,
            ],
        ),
        (
            Section::Network,
            vec![
                SettingsPage::Proxy,
                SettingsPage::Authentication,
                SettingsPage::Security,
            ],
        ),
    ];
    if !integrations.is_empty() {
        sections.push((Section::Integrations, integrations));
    }
    sections.push((Section::Tools, tools));
    sections
}

/// Whether `page` has an entry right now.
pub fn is_available(cx: &App, page: SettingsPage) -> bool {
    categories(cx)
        .iter()
        .any(|(_, pages)| pages.contains(&page))
}

/// Opens the folder `path` with the system's file manager. Tests replace it with a recorder.
pub struct OpenFolderHook(pub Rc<dyn Fn(&std::path::Path, &mut App)>);

impl Global for OpenFolderHook {}

pub(crate) fn open_folder(cx: &mut App, path: &std::path::Path) {
    if let Err(err) = std::fs::create_dir_all(path) {
        tracing::warn!("cannot create {}: {err}", path.display());
    }
    match cx.try_global::<OpenFolderHook>().map(|h| h.0.clone()) {
        Some(hook) => hook(path, cx),
        None => cx.open_with_system(path),
    }
}

/// The weak handle of the open Settings view (a second request selects a page instead of stacking dialogs).
struct OpenSettingsView(WeakEntity<SettingsView>);

impl Global for OpenSettingsView {}

/// The open Settings view, if any (cross-feature tests).
#[cfg(all(test, feature = "services"))]
pub(crate) fn open_view(cx: &App) -> Option<Entity<SettingsView>> {
    cx.try_global::<OpenSettingsView>()
        .and_then(|g| g.0.upgrade())
}

/// Opens the Settings dialog, optionally at `page` (ignored when that category is not available). While the
/// dialog is open, a second call only switches the page.
pub fn open(window: &mut Window, cx: &mut App, page: Option<SettingsPage>) {
    let page = page.filter(|p| is_available(cx, *p));
    let existing = cx
        .try_global::<OpenSettingsView>()
        .and_then(|g| g.0.upgrade());
    if let Some(view) = existing
        && window.has_active_dialog(cx)
    {
        if let Some(page) = page {
            view.update(cx, |view, cx| view.select_page(page, cx));
        }
        return;
    }
    let view = cx.new(|cx| SettingsView::new(page.unwrap_or(SettingsPage::Appearance), window, cx));
    cx.set_global(OpenSettingsView(view.downgrade()));
    super::open_layer(
        window,
        cx,
        Rc::new(move |dialog, window, _| {
            let (w, h) = frame_size(window);
            let viewport_h = window.viewport_size().height.as_f32();
            let view = view.clone();
            let closing = view.downgrade();
            dialog
                .entrance(DialogEntrance::None)
                .close_button(false)
                .overlay_closable(false)
                // Enter belongs to the fields; the kit's default OK must not close the dialog.
                .on_ok(|_, _, _| false)
                .p_0()
                .border_0()
                .bg(transparent_black())
                .shadow_none()
                .w(px(w))
                .margin_top(px(((viewport_h - h) / 2.).max(KIT_EDGE_MARGIN)))
                .on_close(move |_, _, cx| {
                    closing.update(cx, |view, cx| view.flush_pending(cx)).ok();
                })
                .content(move |content, _, _| content.child(view.clone()))
        }),
    );
}

/// The kit keeps this much of the window clear around a dialog (`spacing_tokens().lg`).
const KIT_EDGE_MARGIN: f32 = 16.;

/// The frame size in px: 1000 × 720 dp, but at most the window minus the kit's 16 px edge margin on every side
/// (a 720 px high window shows a 688 px high dialog).
fn frame_size(window: &Window) -> (f32, f32) {
    let viewport = window.viewport_size();
    let room = |v: f32| (v - 2. * KIT_EDGE_MARGIN).max(0.);
    (
        dpx(sm::DIALOG_W, window)
            .as_f32()
            .min(room(viewport.width.as_f32())),
        dpx(sm::DIALOG_H, window)
            .as_f32()
            .min(room(viewport.height.as_f32())),
    )
}

/// Registers the runtime rebuild on Git executable, proxy and security changes.
pub fn register(cx: &mut App) {
    register_runtime_watch(cx);
}

/// Applies a dropdown choice to the view.
type Pick = Rc<dyn Fn(&mut SettingsView, &mut Window, &mut Context<SettingsView>)>;

/// The state of the proxy password row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PasswordState {
    /// The keychain has not answered yet.
    Unknown,
    /// Whether a password is stored.
    Stored(bool),
    /// The keychain cannot be used (reason).
    Unavailable(String),
}

/// The entities behind the text and number rows.
struct Fields {
    clone_dir: Entity<OutlinedField>,
    git_path: Entity<OutlinedField>,
    terminal_path: Entity<OutlinedField>,
    date_format: Entity<OutlinedField>,
    undo_entries: Entity<NumberField>,
    undo_days: Entity<NumberField>,
    proxy_host: Entity<OutlinedField>,
    proxy_port: Entity<NumberField>,
    proxy_user: Entity<OutlinedField>,
    proxy_password: Entity<OutlinedField>,
    custom_theme: Entity<ThemeEditor>,
}

/// The dialog content.
pub struct SettingsView {
    page: SettingsPage,
    open_select: Option<&'static str>,
    select_bounds: HashMap<&'static str, BoundsCell>,
    fields: Fields,
    password: PasswordState,
    /// The window's viewport at the last render (the dropdown backdrop covers it).
    viewport: Size<Pixels>,
    theme_task: Option<Task<()>>,
    password_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl SettingsView {
    /// The dialog content showing `page`.
    pub fn new(page: SettingsPage, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let s = settings::get_or_default(cx);
        let text_field = |key: &str, window: &mut Window, cx: &mut Context<Self>| {
            let id = ids::field(key);
            cx.new(|cx| OutlinedField::new(id, window, cx))
        };
        let clone_dir = text_field("clone-dir", window, cx);
        let git_path = text_field("git-path", window, cx);
        let terminal_path = text_field("terminal-path", window, cx);
        let date_format = text_field("date-format", window, cx);
        let proxy_host = text_field("proxy-host", window, cx);
        let proxy_user = text_field("proxy-user", window, cx);
        let proxy_password =
            cx.new(|cx| OutlinedField::password(ids::field("proxy-password"), window, cx));
        let undo_entries = cx.new(|cx| NumberField::new(ids::field("undo-entries"), window, cx));
        let undo_days = cx.new(|cx| NumberField::new(ids::field("undo-days"), window, cx));
        let proxy_port = cx.new(|cx| NumberField::new(ids::field("proxy-port"), window, cx));
        let custom_theme = cx.new(|cx| ThemeEditor::new(ids::THEME_EDITOR, window, cx));

        let path_text = |p: &Option<std::path::PathBuf>| {
            p.as_ref()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        clone_dir.update(cx, |f, cx| {
            f.set_text(&path_text(&s.clone_default_directory), window, cx)
        });
        git_path.update(cx, |f, cx| f.set_text(&path_text(&s.git_path), window, cx));
        terminal_path.update(cx, |f, cx| f.set_disabled(true, cx));
        date_format.update(cx, |f, cx| f.set_text(&s.date_custom_format, window, cx));
        proxy_host.update(cx, |f, cx| f.set_text(&s.proxy_host, window, cx));
        proxy_user.update(cx, |f, cx| f.set_text(&s.proxy_user, window, cx));
        undo_entries.update(cx, |f, cx| {
            f.set_value(s.undo_keep_entries as u64, window, cx)
        });
        undo_days.update(cx, |f, cx| {
            f.set_value(u64::from(s.undo_keep_days), window, cx)
        });
        if s.proxy_port != 0 {
            proxy_port.update(cx, |f, cx| f.set_value(u64::from(s.proxy_port), window, cx));
        }
        custom_theme.update(cx, |f, cx| f.set_text(&s.custom_theme, window, cx));

        let mut subscriptions = vec![cx.observe_global::<SettingsGlobal>(|this, cx| {
            this.sync_fields(cx);
            cx.notify();
        })];
        // Escape with an open dropdown closes only the popup (09 §4.0). The kit dialog binds Escape to its Cancel
        // action, and bindings are matched before key listeners, so the popup takes the key in an interceptor.
        let this = cx.weak_entity();
        let home = window.window_handle();
        subscriptions.push(cx.intercept_keystrokes(move |event, window, cx| {
            if event.keystroke.key != "escape" || window.window_handle() != home {
                return;
            }
            let closed = this
                .update(cx, |view, cx| {
                    let open = view.open_select.take().is_some();
                    if open {
                        cx.notify();
                    }
                    open
                })
                .unwrap_or(false);
            if closed {
                cx.stop_propagation();
            }
        }));
        subscriptions.push(cx.subscribe_in(
            &clone_dir,
            window,
            |_, field, _: &FieldEvent, _, cx| {
                let path = path_of(&field.read(cx).text(cx));
                settings::update(cx, |s| s.clone_default_directory = path);
            },
        ));
        subscriptions.push(cx.subscribe_in(
            &git_path,
            window,
            |_, field, _: &FieldEvent, _, cx| {
                let path = path_of(&field.read(cx).text(cx));
                settings::update(cx, |s| s.git_path = path);
            },
        ));
        subscriptions.push(cx.subscribe_in(
            &date_format,
            window,
            |_, field, _: &FieldEvent, _, cx| {
                let pattern = field.read(cx).text(cx).to_string();
                let valid = valid_date_pattern(&pattern);
                field.update(cx, |f, cx| f.set_error(!valid, cx));
                if valid {
                    settings::update(cx, |s| s.date_custom_format = pattern);
                }
                cx.notify();
            },
        ));
        subscriptions.push(cx.subscribe_in(
            &proxy_host,
            window,
            |_, field, _: &FieldEvent, _, cx| {
                let host = field.read(cx).text(cx).to_string();
                settings::update(cx, |s| s.proxy_host = host);
            },
        ));
        subscriptions.push(cx.subscribe_in(
            &proxy_user,
            window,
            |_, field, _: &FieldEvent, _, cx| {
                // Gitnuro wrote the login into the password key (09 §5.8); the login has its own key.
                let user = field.read(cx).text(cx).to_string();
                settings::update(cx, |s| s.proxy_user = user);
            },
        ));
        subscriptions.push(cx.subscribe_in(
            &proxy_password,
            window,
            |this, _, _: &FieldEvent, _, cx| this.password_changed(cx),
        ));
        subscriptions.push(cx.subscribe_in(
            &custom_theme,
            window,
            |this, _, _: &FieldEvent, _, cx| this.theme_text_changed(cx),
        ));
        subscriptions.push(cx.subscribe_in(
            &undo_entries,
            window,
            |_, field, event: &NumberEvent, _, cx| match event {
                NumberEvent::Changed(Some(v)) if *v >= 1 => {
                    let v = *v as usize;
                    settings::update(cx, |s| s.undo_keep_entries = v);
                }
                NumberEvent::Changed(Some(_)) => field.update(cx, |f, cx| f.reject(cx)),
                NumberEvent::Changed(None) => {}
            },
        ));
        subscriptions.push(cx.subscribe_in(
            &undo_days,
            window,
            |_, field, event: &NumberEvent, _, cx| match event {
                NumberEvent::Changed(Some(v)) if *v >= 1 => {
                    let v = *v as u32;
                    settings::update(cx, |s| s.undo_keep_days = v);
                }
                NumberEvent::Changed(Some(_)) => field.update(cx, |f, cx| f.reject(cx)),
                NumberEvent::Changed(None) => {}
            },
        ));
        subscriptions.push(cx.subscribe_in(
            &proxy_port,
            window,
            |_, field, event: &NumberEvent, _, cx| match event {
                NumberEvent::Changed(value) => match u16::try_from(value.unwrap_or(0)) {
                    Ok(port) => settings::update(cx, |s| s.proxy_port = port),
                    Err(_) => field.update(cx, |f, cx| f.reject(cx)),
                },
            },
        ));

        let mut view = Self {
            page,
            open_select: None,
            select_bounds: HashMap::new(),
            fields: Fields {
                clone_dir,
                git_path,
                terminal_path,
                date_format,
                undo_entries,
                undo_days,
                proxy_host,
                proxy_port,
                proxy_user,
                proxy_password,
                custom_theme,
            },
            password: PasswordState::Unknown,
            viewport: window.viewport_size(),
            theme_task: None,
            password_task: None,
            _subscriptions: subscriptions,
        };
        view.sync_fields(cx);
        view.probe_password(cx);
        view
    }

    /// Applies the edits that wait for their quiet period; the dialog is closing (Accept or Escape), so closing
    /// inside the quiet period must not lose them.
    fn flush_pending(&mut self, cx: &mut App) {
        if self.theme_task.take().is_some() {
            self.apply_theme_text(cx);
        }
        if self.password_task.take().is_some() {
            let text = self.fields.proxy_password.read(cx).text(cx).to_string();
            if text.is_empty() {
                return;
            }
            let store = runtime::secret_store(cx);
            cx.spawn(async move |cx| {
                let result = cx
                    .background_executor()
                    .spawn(async move { (store.store)(&text) })
                    .await;
                cx.update(|cx| match result {
                    Ok(()) => rebuild_now(cx),
                    Err(err) => crate::app_view::show_toast(
                        cx,
                        None,
                        crate::notifications::ToastKind::Error,
                        format!("Cannot store the proxy password: {err}"),
                        None,
                    ),
                });
            })
            .detach();
        }
    }

    /// The selected category.
    pub fn page(&self) -> SettingsPage {
        self.page
    }

    /// Selects `page`.
    pub fn select_page(&mut self, page: SettingsPage, cx: &mut Context<Self>) {
        if self.page != page {
            self.page = page;
            self.open_select = None;
            cx.notify();
        }
    }

    /// The state of the proxy password row.
    pub fn password_state(&self) -> &PasswordState {
        &self.password
    }

    /// Whether the open dropdown is `key`.
    pub fn is_select_open(&self, key: &str) -> bool {
        self.open_select == Some(key)
    }

    /// Brings enabled states and error borders in line with the settings.
    fn sync_fields(&mut self, cx: &mut Context<Self>) {
        let s = settings::get_or_default(cx);
        let f = &self.fields;
        f.date_format
            .update(cx, |f, cx| f.set_disabled(s.date_use_system, cx));
        f.proxy_host
            .update(cx, |f, cx| f.set_disabled(!s.use_proxy, cx));
        f.proxy_port
            .update(cx, |f, cx| f.set_disabled(!s.use_proxy, cx));
        let auth = s.use_proxy && s.proxy_auth;
        f.proxy_user.update(cx, |f, cx| f.set_disabled(!auth, cx));
        let password_usable = !matches!(self.password, PasswordState::Unavailable(_));
        f.proxy_password
            .update(cx, |f, cx| f.set_disabled(!(auth && password_usable), cx));
        let theme_invalid = s.theme == crate::theme::ThemeKind::Custom
            && crate::theme::custom::parse(&s.custom_theme).is_err();
        f.custom_theme
            .update(cx, |f, cx| f.set_error(theme_invalid, cx));
    }

    fn theme_text_changed(&mut self, cx: &mut Context<Self>) {
        self.theme_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(sm::THEME_DEBOUNCE_MS))
                .await;
            this.update(cx, |this, cx| {
                this.theme_task = None;
                this.apply_theme_text(cx);
            })
            .ok();
        }));
    }

    /// Stores the editor's text as the custom theme. Unusable JSON falls back to the Dark theme (the theme observer
    /// does that) and shows the Error toast of 21 §3.1.
    fn apply_theme_text(&mut self, cx: &mut App) {
        let text = self.fields.custom_theme.read(cx).text(cx).to_string();
        if text == settings::get_or_default(cx).custom_theme {
            return;
        }
        let usable = crate::theme::custom::parse(&text).is_ok();
        settings::update(cx, |s| s.custom_theme = text);
        let custom = settings::get_or_default(cx).theme == crate::theme::ThemeKind::Custom;
        if !usable && custom {
            crate::app_view::show_toast(
                cx,
                None,
                crate::notifications::ToastKind::Error,
                "Failed trying to save the custom theme",
                None,
            );
        }
    }

    /// Selects the theme. Choosing Custom with no text yet starts from the current theme's full export.
    fn choose_theme(
        &mut self,
        kind: crate::theme::ThemeKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let needs_template = kind == crate::theme::ThemeKind::Custom
            && settings::get_or_default(cx).custom_theme.trim().is_empty();
        if needs_template {
            let json = crate::theme::custom::export(cx.tokens());
            self.fields
                .custom_theme
                .update(cx, |f, cx| f.set_text(&json, window, cx));
            settings::update(cx, |s| {
                s.theme = kind;
                s.custom_theme = json;
            });
        } else {
            settings::update(cx, |s| s.theme = kind);
            // A stored custom theme that cannot be used falls back to Dark (the theme observer); say so.
            let unusable = kind == crate::theme::ThemeKind::Custom
                && crate::theme::custom::parse(&settings::get_or_default(cx).custom_theme).is_err();
            if unusable {
                crate::app_view::show_toast(
                    cx,
                    None,
                    crate::notifications::ToastKind::Error,
                    "Failed trying to save the custom theme",
                    None,
                );
            }
        }
    }

    fn password_changed(&mut self, cx: &mut Context<Self>) {
        // An empty field never deletes the stored secret: only the Remove link does.
        if self.fields.proxy_password.read(cx).text(cx).is_empty() {
            self.password_task = None;
            return;
        }
        self.password_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(sm::PASSWORD_DEBOUNCE_MS))
                .await;
            let Ok((text, store)) = this.update(cx, |this, cx| {
                (
                    this.fields.proxy_password.read(cx).text(cx).to_string(),
                    runtime::secret_store(cx),
                )
            }) else {
                return;
            };
            let result = cx
                .background_executor()
                .spawn(async move { (store.store)(&text) })
                .await;
            this.update(cx, |this, cx| {
                this.password_task = None;
                match result {
                    Ok(()) => {
                        this.password = PasswordState::Stored(true);
                        rebuild_now(cx);
                    }
                    Err(err) => {
                        this.password = PasswordState::Unavailable(err.to_string());
                        crate::app_view::show_toast(
                            cx,
                            None,
                            crate::notifications::ToastKind::Error,
                            format!("Cannot store the proxy password: {err}"),
                            None,
                        );
                    }
                }
                this.sync_fields(cx);
                cx.notify();
            })
            .ok();
        }));
    }

    /// Deletes the stored proxy password (the Remove link) and empties the field.
    fn remove_password(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.password_task = None;
        self.fields
            .proxy_password
            .update(cx, |f, cx| f.set_text("", window, cx));
        let store = runtime::secret_store(cx);
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { (store.remove)() })
                .await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(()) => {
                        this.password = PasswordState::Stored(false);
                        rebuild_now(cx);
                    }
                    Err(err) => crate::app_view::show_toast(
                        cx,
                        None,
                        crate::notifications::ToastKind::Error,
                        format!("Cannot remove the proxy password: {err}"),
                        None,
                    ),
                }
                this.sync_fields(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn probe_password(&mut self, cx: &mut Context<Self>) {
        let store = runtime::secret_store(cx);
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { (store.exists)() })
                .await;
            this.update(cx, |this, cx| {
                this.password = match result {
                    Ok(stored) => PasswordState::Stored(stored),
                    Err(err) => PasswordState::Unavailable(err.to_string()),
                };
                this.sync_fields(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// A dropdown (09 §0.3 `DropDownMenuText`): a 180 dp box showing `current`; clicking opens the options.
    ///
    /// The popup is drawn from inside the dialog: the window's menu layer lies underneath the dialog layer. Deferred
    /// draws are painted by priority across the window and the kit dialog's own priority is 10 and up, so the popup
    /// takes the kit's `POPUP_PRIORITY`: with a lower one it is hidden under the dialog yet still takes the clicks.
    fn select(
        &mut self,
        key: &'static str,
        current: impl Into<SharedString>,
        options: Vec<(SharedString, Pick)>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = *cx.tokens();
        let bounds = self
            .select_bounds
            .entry(key)
            .or_insert_with(|| Rc::new(Cell::new(Default::default())))
            .clone();
        let open = self.open_select == Some(key);
        let current: SharedString = current.into();
        let trigger = div()
            .id(ids::select(key))
            .w_full()
            .flex()
            .flex_row()
            .items_center()
            // Gitnuro's v8 h12 padding with the 2 dp border drawn over it; a GPUI border takes layout space.
            .py(dp(6.))
            .px(dp(10.))
            .rounded(dp(metrics::RADIUS))
            .border(dp(2.))
            .border_color(tokens.dialog_border())
            .bg(tokens.background)
            .cursor_pointer()
            .hover(move |s| s.bg(with_layer(tokens.background, tokens.hover_layer())))
            .child(
                div()
                    .flex_1()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_body2()
                    .text_color(tokens.on_background)
                    .child(current),
            )
            .child(icon(AppIcon::ExpandMore, 24., tokens.on_background))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.open_select = if this.open_select == Some(key) {
                    None
                } else {
                    Some(key)
                };
                cx.notify();
            }))
            .test_support();
        let popup = open.then(|| {
            // The popup menu chrome of 09 §4.0 without the icon column (as `MenuHost` draws it): items h12 v8,
            // no selected state, hover and press layers.
            let rows: Vec<AnyElement> = options
                .into_iter()
                .enumerate()
                .map(|(index, (label, pick))| {
                    div()
                        .id(ids::select_option(key, index))
                        .w_full()
                        .px(dp(12.))
                        .py(dp(metrics::MENU_ITEM_PAD))
                        .text_body2()
                        .text_color(tokens.on_background)
                        .hover(move |s| s.bg(tokens.hover_layer()))
                        .active(move |s| s.bg(tokens.press_layer()))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_select = None;
                            pick(this, window, cx);
                            cx.notify();
                        }))
                        .child(label)
                        .test_support()
                        .into_any_element()
                })
                .collect();
            let card = observed(ids::select_popup(key))
                .min_w(dp(metrics::MENU_MIN_W))
                .flex()
                .flex_col()
                .rounded(dp(metrics::RADIUS))
                .bg(tokens.background)
                .overflow_hidden()
                .shadow(vec![BoxShadow {
                    color: tokens.menu_shadow(),
                    offset: point(px(0.), px(2.)),
                    blur_radius: px(5.),
                    spread_radius: px(1.),
                    inset: false,
                }])
                .when(!tokens.is_light, |m| {
                    m.border(dp(2.)).border_color(tokens.popup_border())
                })
                .children(rows);
            let at = bounds.get().bottom_left();
            let viewport = self.viewport;
            // The backdrop covers the whole window and closes the popup; presses inside the card must not reach it.
            deferred(
                anchored().position(point(px(0.), px(0.))).child(
                    div()
                        .w(viewport.width)
                        .h(viewport.height)
                        .occlude()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _: &MouseDownEvent, _, cx| {
                                this.open_select = None;
                                cx.notify();
                            }),
                        )
                        .child(
                            anchored().position(at).snap_to_window().child(
                                div()
                                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation()
                                    })
                                    .child(card),
                            ),
                        ),
                ),
            )
            .with_priority(POPUP_PRIORITY)
        });
        div()
            .relative()
            .flex_none()
            .w(dp(sm::DROPDOWN_W))
            .child(track_bounds(&bounds))
            .child(trigger)
            .children(popup)
            .into_any_element()
    }

    fn left_column(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let tokens = *cx.tokens();
        let mut entries: Vec<AnyElement> = Vec::new();
        for (index, (section, pages)) in categories(cx).into_iter().enumerate() {
            if index > 0 {
                entries.push(div().h(dp(sm::SECTION_GAP)).into_any_element());
            }
            entries.push(
                div()
                    .px(dp(sm::SECTION_PAD_H))
                    .py(dp(sm::SECTION_PAD_V))
                    .text_body2()
                    .semibold()
                    .text_color(tokens.on_background_secondary)
                    .child(section.title().to_uppercase())
                    .into_any_element(),
            );
            for page in pages {
                let selected = page == self.page;
                let base = if selected {
                    tokens.background_selected
                } else {
                    tokens.background
                };
                entries.push(
                    div()
                        .id(ids::entry(page))
                        .w_full()
                        .h(dp(sm::ENTRY_H))
                        .flex_none()
                        .flex()
                        .flex_row()
                        .items_center()
                        .bg(base)
                        .cursor_pointer()
                        .hover(move |s| s.bg(with_layer(base, tokens.hover_layer())))
                        .on_click(cx.listener(move |this, _, _, cx| this.select_page(page, cx)))
                        .child(
                            div()
                                .flex_none()
                                .pl(dp(sm::SECTION_PAD_H))
                                .pr(dp(sm::ENTRY_ICON_GAP))
                                .child(icon(
                                    page.icon(),
                                    sm::ENTRY_ICON,
                                    tokens.on_background_secondary,
                                )),
                        )
                        .child(
                            div()
                                .text_body1()
                                .text_color(tokens.on_background)
                                .child(page.title()),
                        )
                        .test_support()
                        .into_any_element(),
                );
            }
        }
        observed(ids::LEFT)
            .w(dp(sm::LEFT_W))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .bg(tokens.background)
            .child(
                div()
                    .flex_none()
                    .p(dp(sm::TITLE_PAD))
                    .text_h3()
                    .bold()
                    .text_color(tokens.on_background)
                    .child("Settings"),
            )
            .child(
                div().flex_1().min_h_0().w_full().child(
                    scroll_y("settings-left-scroll")
                        .size_full()
                        .flex()
                        .flex_col()
                        .children(entries),
                ),
            )
            .into_any_element()
    }

    fn page_content(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let rows = match self.page {
            SettingsPage::Appearance => self.appearance_rows(cx),
            SettingsPage::Layout => self.layout_rows(cx),
            SettingsPage::DateTime => self.date_time_rows(cx),
            SettingsPage::Environment => self.environment_rows(cx),
            SettingsPage::Branches => self.branches_rows(cx),
            SettingsPage::RemoteActions => self.remote_rows(cx),
            SettingsPage::Proxy => self.proxy_rows(cx),
            SettingsPage::Authentication => self.authentication_rows(cx),
            SettingsPage::Security => self.security_rows(cx),
            SettingsPage::Terminal => self.terminal_rows(window, cx),
            SettingsPage::Logs => self.logs_rows(cx),
            SettingsPage::Accounts => vec![accounts::page(window, cx)],
            SettingsPage::Ai => vec![ai::page(window, cx)],
            SettingsPage::Updates => vec![updates::page(window, cx)],
        };
        let column = div().w_full().flex().flex_col().children(rows);
        if self.page.scrolls() {
            scroll_y("settings-content-scroll")
                .size_full()
                .child(column)
                .into_any_element()
        } else {
            column.into_any_element()
        }
    }
}

/// `None` for blank text, else the path the text names.
fn path_of(text: &str) -> Option<std::path::PathBuf> {
    let text = text.trim();
    (!text.is_empty()).then(|| std::path::PathBuf::from(text))
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = *cx.tokens();
        let viewport = window.viewport_size();
        let this = cx.weak_entity();
        self.viewport = viewport;
        let (_, frame_h) = frame_size(window);
        let left = self.left_column(cx);
        let content = self.page_content(window, cx);
        observed(ids::DIALOG)
            .relative()
            .w_full()
            .h(px(frame_h))
            .flex()
            .flex_row()
            .overflow_hidden()
            .rounded(dp(metrics::RADIUS_LG))
            .bg(tokens.surface)
            .text_color(tokens.on_background)
            .child(left)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(
                        observed(ids::CONTENT)
                            .flex_1()
                            .min_h_0()
                            .w_full()
                            .pl(dp(sm::CONTENT_PAD_H))
                            .pt(dp(sm::CONTENT_TOP))
                            .pr(dp(sm::CONTENT_PAD_H))
                            .child(content),
                    )
                    .child(
                        div()
                            .flex_none()
                            .w_full()
                            .flex()
                            .flex_row()
                            .justify_end()
                            .pr(dp(sm::CONTENT_PAD_H))
                            .pb(dp(sm::CONTENT_PAD_H))
                            .child(PrimaryButton::new(ids::ACCEPT, "Accept").on_click(
                                move |window, cx| {
                                    this.update(cx, |view, cx| view.flush_pending(cx)).ok();
                                    window.close_dialog(cx);
                                },
                            )),
                    ),
            )
            // The 1 dp border is drawn over the content so that it takes no layout space (Compose draws it over
            // the padding); without handlers the overlay does not block the pointer.
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .rounded(dp(metrics::RADIUS_LG))
                    .border(dp(1.))
                    .border_color(tokens.dialog_border()),
            )
    }
}

/// One row of a settings page: titles on the left (the subtitle wraps), the control on the right
/// (09 §3.18 "Setting row").
#[derive(IntoElement)]
pub(crate) struct SettingRow {
    key: &'static str,
    title: SharedString,
    subtitle: Option<SharedString>,
    notes: Vec<(SharedString, bool)>,
    actions: Vec<AnyElement>,
    control: Option<AnyElement>,
    disabled: bool,
    top_aligned: bool,
}

impl SettingRow {
    /// A row named `key` (element id) with `title`.
    pub(crate) fn new(key: &'static str, title: impl Into<SharedString>) -> Self {
        Self {
            key,
            title: title.into(),
            subtitle: None,
            notes: Vec::new(),
            actions: Vec::new(),
            control: None,
            disabled: false,
            top_aligned: false,
        }
    }

    /// Sets the explanation under the title.
    pub(crate) fn subtitle(mut self, text: impl Into<SharedString>) -> Self {
        self.subtitle = Some(text.into());
        self
    }

    /// Adds a further line under the subtitle; `error` draws it in the error colour.
    pub(crate) fn note(mut self, text: impl Into<SharedString>, error: bool) -> Self {
        self.notes.push((text.into(), error));
        self
    }

    /// Adds a clickable line (see [`text_link`]) under the notes.
    pub(crate) fn action(mut self, link: impl IntoElement) -> Self {
        self.actions.push(link.into_any_element());
        self
    }

    /// Sets the control on the right.
    pub(crate) fn control(mut self, control: impl IntoElement) -> Self {
        self.control = Some(control.into_any_element());
        self
    }

    /// Dims the titles to alpha 0.6 (the control is disabled by its owner).
    pub(crate) fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Aligns the control with the top of the row (multi-line controls).
    pub(crate) fn top_aligned(mut self) -> Self {
        self.top_aligned = true;
        self
    }
}

impl RenderOnce for SettingRow {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let tokens = *cx.tokens();
        let alpha = if self.disabled {
            sm::DISABLED_ALPHA
        } else {
            1.
        };
        let top = self.top_aligned;
        observed(ids::row(self.key))
            .w_full()
            .flex()
            .flex_row()
            .when(top, |r| r.items_start())
            .when(!top, |r| r.items_center())
            .py(dp(sm::ROW_PAD_V))
            .child(
                observed(ids::titles(self.key))
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .pr(dp(sm::CONTROL_GAP))
                    .opacity(alpha)
                    .child(
                        div()
                            .text_body1()
                            .medium()
                            .text_color(tokens.on_background)
                            .child(self.title),
                    )
                    .children(self.subtitle.map(|text| {
                        div()
                            .pt(dp(sm::SUBTITLE_GAP))
                            .text_body2()
                            .text_color(tokens.on_background_secondary)
                            .child(text)
                    }))
                    .children(self.notes.into_iter().map(|(text, error)| {
                        div()
                            .pt(dp(sm::SUBTITLE_GAP))
                            .text_body2()
                            .text_color(if error {
                                tokens.error
                            } else {
                                tokens.on_background_secondary
                            })
                            .child(text)
                    }))
                    .children(
                        self.actions.into_iter().map(|link| {
                            div().pt(dp(sm::SUBTITLE_GAP)).flex().flex_row().child(link)
                        }),
                    ),
            )
            .children(self.control.map(|control| div().flex_none().child(control)))
    }
}

/// A text link inside a settings row: `primary` text, `primary_variant` on hover.
pub(crate) fn text_link(
    id: impl Into<ElementId>,
    text: &'static str,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
    cx: &App,
) -> impl IntoElement {
    let tokens = cx.tokens();
    let hover = tokens.primary_variant;
    div()
        .id(id)
        .flex_none()
        .whitespace_nowrap()
        .text_body2()
        .text_color(tokens.primary)
        .cursor_pointer()
        .hover(move |s| s.text_color(hover))
        .on_click(move |_, window, cx| on_click(window, cx))
        .child(text)
        .test_support()
}

#[cfg(test)]
mod tests;
