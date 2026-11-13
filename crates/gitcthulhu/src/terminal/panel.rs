//! The terminal panel of a repository tab (21 §5.2, 18 §5.3): below the three-pane split, above the frame divider.
//! An 8 dp resize gutter, then the box (x 8..W-8) with the 34 dp header (terminal tabs, "+", hide) and the active
//! terminal, then 8 dp. Its height (header + body) is persisted in `UiState::terminal_height` when a drag ends.
//!
//! Terminals stay alive while the panel is hidden (they build no frames then); the first show opens one with the
//! default shell at the repository's worktree; the last shell exiting hides the panel; dropping the panel (with its
//! tab) drops the terminals, which ends their shells and everything those started.

use std::path::{Path, PathBuf};

use but_tty::{Shell, ShellSpec, Status, Terminal as Tty};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, Focusable as _, Hsla,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, TestSupportExt as _, Window, div,
};

use super::colors::TerminalColors;
use super::ids;
use super::shell;
use super::view::{OPENING, TerminalEvent, TerminalView};
use crate::backend::prompt::ConfirmRequest;
use crate::icons::{AppIcon, icon};
use crate::menus::{self, MenuItem};
use crate::metrics::{self, BASE_REM, dp};
use crate::notifications::ToastKind;
use crate::settings::{self, TerminalShell};
use crate::tab::TabCtx;
use crate::theme::{ActiveTokens as _, TextStyleExt as _};
use crate::widgets::{HeaderBar34, TooltipExt as _, VResizer, observed};

/// The space around the box: the resize gutter above it, the padding below it and the margins at its sides, in dp.
pub(super) const PANEL_PAD: f32 = 8.;
/// The "+" and hide buttons of the header (21 §5.2).
pub(super) const HEADER_BUTTON: f32 = 34.;
/// Icon size of the header buttons.
const HEADER_ICON: f32 = 18.;
/// The close button's icon in a terminal tab, and its padding.
const TAB_CLOSE_ICON: f32 = 14.;
const TAB_CLOSE_PAD: f32 = 8.;
/// Where a tab's status dot starts, and the gap between dot and title (title at x + 28).
const TAB_DOT_LEFT: f32 = 12.;
const TAB_DOT_GAP: f32 = 8.;
/// The selected tab's bottom line.
const TAB_LINE: f32 = 2.;

/// What the panel tells its tab's views.
#[derive(Clone, Debug, PartialEq)]
pub enum PanelEvent {
    VisibilityChanged(bool),
    StatusChanged,
}

struct Entry {
    view: Entity<TerminalView>,
    title: SharedString,
    status: Status,
    _subscription: Subscription,
}

/// The terminals of one repository tab.
pub struct TerminalPanel {
    ctx: TabCtx,
    workdir: PathBuf,
    terminals: Vec<Entry>,
    active: usize,
    visible: bool,
    /// The height while the gutter is dragged (otherwise the persisted one applies).
    drag_height: Option<f32>,
    /// Shells being started on the background executor.
    starting: usize,
    /// Whether the panel's repository tab is the window's selected one.
    tab_selected: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<PanelEvent> for TerminalPanel {}

impl TerminalPanel {
    /// The panel of the tab of `ctx`, whose terminals start in `workdir`.
    pub(super) fn create(
        ctx: TabCtx,
        workdir: PathBuf,
        _window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        cx.new(|cx| {
            let mut subscriptions = Vec::new();
            if let Some(app) = cx
                .try_global::<crate::app_view::AppViewGlobal>()
                .and_then(|global| global.0.upgrade())
            {
                subscriptions.push(cx.observe(&app, |panel: &mut Self, app, cx| {
                    let selected = app
                        .read(cx)
                        .active_tab()
                        .is_some_and(|tab| tab.entity_id() == panel.ctx.tab.entity_id());
                    if selected != panel.tab_selected {
                        panel.tab_selected = selected;
                        panel.sync_shown(cx);
                    }
                }));
            }
            Self {
                ctx,
                workdir,
                terminals: Vec::new(),
                active: 0,
                visible: false,
                drag_height: None,
                starting: 0,
                tab_selected: true,
                _subscriptions: subscriptions,
            }
        })
    }

    /// The tab of this panel.
    pub fn ctx(&self) -> &TabCtx {
        &self.ctx
    }

    /// The directory terminals start in (the repository's worktree).
    pub fn workdir(&self) -> &Path {
        &self.workdir
    }

    /// Whether the panel shows.
    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// Whether a shell is being started.
    pub fn is_starting(&self) -> bool {
        self.starting > 0
    }

    /// The terminals, in tab order.
    pub fn terminals(&self) -> impl Iterator<Item = &Entity<TerminalView>> {
        self.terminals.iter().map(|entry| &entry.view)
    }

    /// The index of the selected terminal tab.
    pub fn active_index(&self) -> usize {
        self.active
    }

    /// The selected terminal.
    pub fn active_terminal(&self) -> Option<&Entity<TerminalView>> {
        self.terminals.get(self.active).map(|entry| &entry.view)
    }

    /// The panel's height (header + body) in dp before clamping to the window: the live drag, else the persisted
    /// height.
    pub fn height(&self, cx: &App) -> f32 {
        self.drag_height.unwrap_or_else(|| {
            cx.try_global::<settings::state::UiStateGlobal>()
                .map_or(metrics::TERMINAL_DEFAULT_H, |state| state.0.terminal_height)
        })
    }

    /// Sets and persists the height (header + body) in dp, clamped to the window (QA, tests).
    pub fn set_height(&mut self, height: f32, window: &Window, cx: &mut Context<Self>) {
        let height = clamp_height(height, max_height(window));
        self.drag_height = None;
        settings::state::update(cx, |state| state.terminal_height = height);
        cx.notify();
    }

    /// The most urgent status of the terminals, for the toolbar button's dot.
    pub fn aggregate_status(&self, _cx: &App) -> Option<Status> {
        Status::most_urgent(self.terminals.iter().map(|entry| &entry.status)).cloned()
    }

    /// The question asked before the tab closes while a program in a terminal reports that it is working, or a
    /// program the shell started runs at all (a build, an editor with unsaved work).
    pub fn confirm_close(&self, cx: &App) -> Option<ConfirmRequest> {
        let working = self
            .terminals
            .iter()
            .filter(|entry| {
                entry.status == Status::Working || entry.view.read(cx).program_running()
            })
            .count();
        if working == 0 {
            return None;
        }
        let what = if working == 1 {
            "A program in the terminal is still working.".to_owned()
        } else {
            format!("Programs in {working} terminals are still working.")
        };
        let mut request = ConfirmRequest::new(
            "Close the repository tab?",
            format!("{what} Closing the tab ends them."),
        );
        request.confirm_label = "Close tab".into();
        request.destructive = true;
        Some(request)
    }

    /// Shows the panel if hidden, else hides it.
    pub fn toggle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.visible {
            self.hide(window, cx);
        } else {
            self.show(window, cx);
        }
    }

    /// Shows the panel and focuses the active terminal; opens one with the default shell when there is none.
    pub fn show(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.visible {
            self.set_visible(true, cx);
        }
        if self.terminals.is_empty() {
            if self.starting == 0 {
                self.open_terminal(None, window, cx);
            }
        } else {
            self.focus_active(window, cx);
        }
    }

    /// Hides the panel (its terminals keep running) and gives the keys back to the repository frame.
    pub fn hide(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.hide_and_refocus(true, window, cx);
    }

    /// Hides the panel; gives the keys to the repository frame only when `refocus` (a panel hiding on its own, as
    /// its last shell exits, must not take the keys from wherever they are, possibly another repository tab).
    fn hide_and_refocus(&mut self, refocus: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !self.visible {
            return;
        }
        self.set_visible(false, cx);
        if refocus && let Some(tab) = self.ctx.tab.upgrade() {
            let focus = tab.read(cx).focus.clone();
            window.focus(&focus, cx);
        }
    }

    fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        self.visible = visible;
        self.sync_shown(cx);
        self.ctx.ui.update(cx, |ui, cx| {
            ui.terminal_visible = visible;
            cx.notify();
        });
        cx.emit(PanelEvent::VisibilityChanged(visible));
        cx.notify();
    }

    /// Only the selected terminal of a visible panel of the selected tab draws frames.
    fn sync_shown(&mut self, cx: &mut Context<Self>) {
        let shown = self.visible && self.tab_selected;
        let active = self.active;
        for (ix, entry) in self.terminals.iter().enumerate() {
            let on_screen = shown && ix == active;
            entry.view.update(cx, |view, cx| {
                if view.is_shown() != on_screen {
                    view.set_shown(on_screen, cx);
                }
            });
        }
    }

    fn focus_active(&self, window: &mut Window, cx: &mut App) {
        if let Some(view) = self.active_terminal() {
            let focus = view.read(cx).focus_handle(cx);
            window.focus(&focus, cx);
        }
    }

    /// Selects terminal tab `ix` and focuses it.
    pub fn select(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.terminals.len() {
            return;
        }
        self.active = ix;
        self.sync_shown(cx);
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Opens a terminal with `choice` (default: the Shell setting) in the worktree, as a new tab after the others.
    /// A shell that was not found falls back to the automatic one, with a warning toast.
    pub fn open_terminal(
        &mut self,
        choice: Option<TerminalShell>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let choice = choice.unwrap_or_else(|| settings::get_or_default(cx).terminal_shell);
        let detected = shell::detected();
        let spec = shell::resolve(&choice, detected).unwrap_or_else(|why| {
            let fallback = shell::automatic(detected);
            crate::app_view::show_toast(
                cx,
                Some(self.ctx.id),
                ToastKind::Warning,
                format!(
                    "{why}. The terminal starts {} instead.",
                    program_name(&fallback)
                ),
                None,
            );
            fallback
        });
        let size = self
            .active_terminal()
            .map_or(OPENING, |view| view.read(cx).size());
        self.start(spec, size, window, cx);
    }

    fn start(
        &mut self,
        spec: ShellSpec,
        size: but_tty::Size,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.starting += 1;
        cx.notify();
        let cwd = self.workdir.clone();
        let named = ShellName {
            program: spec.program.to_string_lossy().into_owned(),
            name: shell_name(&spec).into(),
            kind: spec.kind,
        };
        let spawn = cx
            .background_executor()
            .spawn(async move { Shell::spawn_with(&spec, &cwd, size, None) });
        cx.spawn_in(window, async move |this, cx| {
            let shell = spawn.await;
            this.update_in(cx, |panel, window, cx| {
                panel.starting -= 1;
                panel.started(shell, named, window, cx);
            })
            .ok();
        })
        .detach();
    }

    fn started(
        &mut self,
        shell: anyhow::Result<Shell>,
        named: ShellName,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let theme = cx.theme_g();
        let colors = TerminalColors::of(&theme.tokens, &theme.lanes);
        let started = shell.and_then(|shell| {
            Tty::new(shell, &colors.colors, colors.background, colors.foreground)
        });
        let (tty, updates) = match started {
            Ok(started) => started,
            Err(err) => {
                tracing::error!(
                    "cannot start a terminal in {}: {err:#}",
                    self.workdir.display()
                );
                crate::app_view::show_toast(
                    cx,
                    Some(self.ctx.id),
                    ToastKind::Error,
                    format!("Couldn't start the terminal: {err:#}"),
                    None,
                );
                if self.terminals.is_empty() && self.starting == 0 {
                    self.hide_and_refocus(false, window, cx);
                }
                cx.notify();
                return;
            }
        };
        let view = cx.new(|cx| TerminalView::new(tty, updates, named, colors, cx));
        let subscription = cx.subscribe_in(&view, window, Self::on_terminal_event);
        let title = view.read(cx).title();
        self.terminals.push(Entry {
            view,
            title,
            status: Status::Idle,
            _subscription: subscription,
        });
        self.active = self.terminals.len() - 1;
        self.sync_shown(cx);
        if self.visible {
            self.focus_active(window, cx);
        }
        cx.notify();
    }

    fn on_terminal_event(
        &mut self,
        view: &Entity<TerminalView>,
        event: &TerminalEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ix) = self.terminals.iter().position(|entry| entry.view == *view) else {
            return;
        };
        match event {
            TerminalEvent::Retitled(title) => {
                self.terminals[ix].title = title.clone();
                cx.notify();
            }
            TerminalEvent::Restatused(status) => {
                self.terminals[ix].status = status.clone();
                cx.emit(PanelEvent::StatusChanged);
                cx.notify();
            }
            TerminalEvent::Exited => self.remove(ix, window, cx),
        }
    }

    /// Closes terminal tab `ix`: its shell and what that started end.
    pub fn close_terminal(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix < self.terminals.len() {
            self.remove(ix, window, cx);
        }
    }

    fn remove(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let had_focus = self.terminals[ix]
            .view
            .read(cx)
            .focus_handle(cx)
            .contains_focused(window, cx);
        self.terminals.remove(ix);
        if self.active > ix || self.active >= self.terminals.len() {
            self.active = self.active.saturating_sub(1);
        }
        cx.emit(PanelEvent::StatusChanged);
        if self.terminals.is_empty() {
            if self.starting == 0 {
                self.hide_and_refocus(had_focus, window, cx);
            }
        } else {
            self.sync_shown(cx);
            if had_focus {
                self.focus_active(window, cx);
            }
        }
        cx.notify();
    }

    fn shell_menu(&self, cx: &mut Context<Self>) -> Vec<MenuItem> {
        let weak = cx.weak_entity();
        let mut choices: Vec<(SharedString, TerminalShell)> = shell::detected()
            .iter()
            .filter(|shell| shell.choice != TerminalShell::Auto)
            .map(|shell| (shell::label(&shell.choice).into(), shell.choice.clone()))
            .collect();
        if choices.is_empty() {
            choices.push(("Default shell".into(), TerminalShell::Auto));
        }
        if let TerminalShell::Custom(line) = settings::get_or_default(cx).terminal_shell {
            choices.push((
                format!("Custom: {line}").into(),
                TerminalShell::Custom(line),
            ));
        }
        choices
            .into_iter()
            .enumerate()
            .map(|(ix, (label, choice))| {
                let weak = weak.clone();
                MenuItem::action(format!("terminal-shell-{ix}"), label, move |window, cx| {
                    let choice = choice.clone();
                    weak.update(cx, |panel, cx| {
                        panel.open_terminal(Some(choice), window, cx)
                    })
                    .ok();
                })
                .with_icon(AppIcon::Terminal)
            })
            .collect()
    }

    fn header(&self, cx: &mut Context<Self>) -> AnyElement {
        let tokens = *cx.tokens();
        let tabs: Vec<_> = self
            .terminals
            .iter()
            .enumerate()
            .map(|(ix, entry)| {
                let selected = ix == self.active;
                let dot = status_color(&entry.status, cx);
                let group: SharedString = format!("terminal-tab-{ix}").into();
                div()
                    .id((ids::TAB, ix))
                    .group(group.clone())
                    .relative()
                    .flex_none()
                    .h_full()
                    .min_w(dp(metrics::TERMINAL_TAB_MIN_W))
                    .max_w(dp(metrics::TERMINAL_TAB_MAX_W))
                    .flex()
                    .flex_row()
                    .items_center()
                    .when(selected, |tab| tab.bg(tokens.background))
                    .hover(move |s| s.bg(tokens.hover_layer()))
                    .cursor_pointer()
                    .on_click(cx.listener(move |panel, _, window, cx| panel.select(ix, window, cx)))
                    .on_mouse_down(
                        MouseButton::Middle,
                        cx.listener(move |panel, _, window, cx| {
                            panel.close_terminal(ix, window, cx);
                            cx.stop_propagation();
                        }),
                    )
                    .child(div().flex_none().w(dp(TAB_DOT_LEFT)))
                    .child(match dot {
                        Some(color) => div()
                            .id((ids::TAB_DOT, ix))
                            .flex_none()
                            .size(dp(metrics::STATUS_DOT))
                            .rounded_full()
                            .bg(color)
                            .test_support()
                            .into_any_element(),
                        None => div()
                            .flex_none()
                            .size(dp(metrics::STATUS_DOT))
                            .into_any_element(),
                    })
                    .child(div().flex_none().w(dp(TAB_DOT_GAP)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .text_body2()
                            .medium()
                            .text_color(tokens.on_background)
                            .child(entry.title.clone()),
                    )
                    .child(
                        div()
                            .id((ids::TAB_CLOSE, ix))
                            .flex_none()
                            .p(dp(TAB_CLOSE_PAD))
                            .flex()
                            .items_center()
                            .justify_center()
                            .when(!selected, |close| close.opacity(0.))
                            .group_hover(group, |s| s.opacity(1.))
                            .on_click(cx.listener(move |panel, _, window, cx| {
                                cx.stop_propagation();
                                panel.close_terminal(ix, window, cx);
                            }))
                            .child(icon(
                                AppIcon::Close,
                                TAB_CLOSE_ICON,
                                tokens.on_background_secondary,
                            ))
                            .instant_tooltip("Close terminal", None)
                            .test_support(),
                    )
                    .when(selected, |tab| {
                        tab.child(
                            div()
                                .absolute()
                                .left_0()
                                .right_0()
                                .bottom_0()
                                .h(dp(TAB_LINE))
                                .bg(tokens.primary_variant),
                        )
                    })
                    .test_support()
            })
            .collect();
        let new_button = header_button(ids::NEW, AppIcon::Add, tokens.on_background, cx)
            .on_click(cx.listener(|panel, _, window, cx| panel.open_terminal(None, window, cx)))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|panel, event: &gpui_kit::MouseDownEvent, window, cx| {
                    let items = panel.shell_menu(cx);
                    menus::open_context_menu(window, cx, event.position, items);
                    cx.stop_propagation();
                }),
            )
            .instant_tooltip("New terminal (right click: choose the shell)", None)
            .test_support();
        let hide_button = header_button(ids::HIDE, AppIcon::ExpandMore, tokens.on_background, cx)
            .mr(dp(PANEL_PAD))
            .on_click(cx.listener(|panel, _, window, cx| panel.hide(window, cx)))
            .instant_tooltip(super::HIDE_TOOLTIP, None)
            .test_support();
        HeaderBar34::new()
            .id(ids::HEADER)
            .children(tabs)
            .child(new_button)
            .child(div().flex_1())
            .child(hide_button)
            .into_any_element()
    }
}

/// A 34 dp square icon button of the header with hover and press layers.
fn header_button(
    id: &'static str,
    role: AppIcon,
    color: Hsla,
    cx: &App,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    let tokens = *cx.tokens();
    div()
        .id(id)
        .flex_none()
        .size(dp(HEADER_BUTTON))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .cursor_pointer()
        .hover(move |s| s.bg(tokens.hover_layer()))
        .active(move |s| s.bg(tokens.press_layer()))
        .child(icon(role, HEADER_ICON, color))
}

/// The status dot colour (21 §5.2): Working `primary_variant`, Blocked `secondary`, Failed `error`, else none.
pub(super) fn status_color(status: &Status, cx: &App) -> Option<Hsla> {
    let tokens = cx.tokens();
    match status {
        Status::Working => Some(tokens.primary_variant),
        Status::Blocked(_) => Some(tokens.secondary),
        Status::Failed(_) => Some(tokens.error),
        Status::Idle | Status::Done => None,
    }
}

/// The shell a terminal started, for its tab title and for quoting dropped paths.
pub(super) struct ShellName {
    /// The program as started (a pseudoconsole titles the terminal with it until the shell sets a title).
    pub program: String,
    /// What the tab shows in its place: "PowerShell 7", "Command Prompt", or the program's file name.
    pub name: SharedString,
    pub kind: but_tty::ShellKind,
}

/// The picker's name of `spec` when it is a detected shell, else its program's file name.
fn shell_name(spec: &ShellSpec) -> String {
    shell::detected()
        .iter()
        .find(|shell| shell.choice != TerminalShell::Auto && shell.spec == *spec)
        .map_or_else(
            || program_name(spec),
            |shell| shell::label(&shell.choice).to_owned(),
        )
}

/// The program `spec` starts, by file name, for messages.
fn program_name(spec: &ShellSpec) -> String {
    Path::new(&spec.program).file_name().map_or_else(
        || spec.program.to_string_lossy().into_owned(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// The largest panel height (header + body) in dp the window allows: the split keeps `TERMINAL_MIN_SPLIT_H` under
/// the toolbar, and the gutter, padding, divider and bottom bar keep theirs.
pub(super) fn max_height(window: &Window) -> f32 {
    let scale = window.rem_size().as_f32() / BASE_REM;
    let window_h = window.viewport_size().height.as_f32() / scale;
    window_h
        - metrics::TAB_STRIP_H
        - metrics::TOOLBAR_H
        - metrics::TERMINAL_MIN_SPLIT_H
        - 2. * PANEL_PAD
        - metrics::FRAME_DIVIDER_H
        - metrics::BOTTOM_BAR_H
}

/// `height` within `[TERMINAL_MIN_H, max]` (the minimum wins in a window too small for it).
pub(super) fn clamp_height(height: f32, max: f32) -> f32 {
    height.min(max).max(metrics::TERMINAL_MIN_H)
}

impl Render for TerminalPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.visible {
            return observed(ids::PANEL)
                .flex_none()
                .w_full()
                .h_0()
                .into_any_element();
        }
        let tokens = *cx.tokens();
        let max = max_height(window);
        let height = clamp_height(self.height(cx), max);
        let background = TerminalColors::of(&tokens, &cx.theme_g().lanes).background;
        let weak = cx.weak_entity();
        let commit = weak.clone();
        let header = self.header(cx);
        let body = div()
            .id(ids::BODY)
            .flex_1()
            .min_h_0()
            .w_full()
            .bg(super::colors::hsla(background))
            .children(self.active_terminal().cloned())
            .test_support();
        observed(ids::PANEL)
            .flex_none()
            .w_full()
            .h(dp(height + 2. * PANEL_PAD))
            .flex()
            .flex_col()
            .child(
                VResizer::new(ids::RESIZER, height, metrics::TERMINAL_MIN_H, max)
                    .on_change(move |height, _, cx| {
                        weak.update(cx, |panel, cx| {
                            panel.drag_height = Some(height);
                            cx.notify();
                        })
                        .ok();
                    })
                    .on_commit(move |height, _, cx| {
                        settings::state::update(cx, |state| state.terminal_height = height);
                        commit
                            .update(cx, |panel, cx| {
                                panel.drag_height = None;
                                cx.notify();
                            })
                            .ok();
                    }),
            )
            .child(
                div()
                    .id(ids::BOX)
                    .flex_none()
                    .h(dp(height))
                    .mx(dp(PANEL_PAD))
                    .flex()
                    .flex_col()
                    .rounded(dp(metrics::RADIUS))
                    .overflow_hidden()
                    .child(header)
                    .child(body)
                    .test_support(),
            )
            .into_any_element()
    }
}
