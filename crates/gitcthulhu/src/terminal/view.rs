//! One terminal: the shell's emulator (`but_tty::Terminal`) drawn as a cell grid, with keys, text input, mouse
//! reporting, selection, clipboard, scrollback and resizing (ported from the old `but-gpui` view, 18 §4.1, §5.4).
//!
//! Repaints are event-driven: the emulator works on its own thread, batches what the shell writes for up to 8 ms and
//! wakes this view once per batch; an idle terminal draws nothing. A hidden terminal builds no frames at all
//! (`set_shown(false)`).

use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use but_tty::{
    Cursor, CursorVisualStyle, ShellKind, Size, Status, Terminal as Tty, Updates, mouse,
};
use futures::StreamExt as _;
use gpui_kit::{
    App, AvailableSpace, Bounds, ClipboardItem, Context, CursorStyle, DispatchPhase,
    ElementInputHandler, EntityInputHandler, EventEmitter, ExternalPaths, FocusHandle, Focusable,
    FontWeight, HitboxBehavior, InteractiveElement as _, IntoElement, KeyDownEvent, Modifiers,
    ModifiersChangedEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ParentElement as _, Pixels, Point, Render, ScrollWheelEvent, SharedString, Styled as _, Task,
    TestSupportExt as _, UTF16Selection, Window, canvas, div, px,
};

use super::colors::{TerminalColors, hsla};
use super::grid::{self, CellMetrics, Grid, PaintStyle};
use super::keys::{self, KeyRoute};
use super::panel::ShellName;
use super::{TERMINAL_CONTEXT, ids};
use crate::fonts::FONT_MONO;
use crate::icons::AppIcon;
use crate::menus::{self, MenuItem};
use crate::metrics::dpx;
use crate::theme::ActiveTokens as _;

/// How often the shell is resized at most while the grid's size keeps changing (as Ghostty coalesces resizes), so
/// dragging the panel's edge has a full-screen program redraw a few dozen times a second rather than every frame.
const RESIZE_EVERY: Duration = Duration::from_millis(25);

/// How often a selection dragged past the top or bottom scrolls a row.
const AUTOSCROLL_EVERY: Duration = Duration::from_millis(15);

/// The smallest and largest text size the setting may give, in dp.
const TEXT_SIZE_RANGE: (f32, f32) = (6., 48.);

/// The text size before the settings are loaded (the setting's default, 21 §7.2).
const DEFAULT_TEXT_SIZE: f32 = 13.;

/// The size a shell starts at, which the terminal's first frame corrects.
pub(super) const OPENING: Size = Size {
    cols: 80,
    rows: 24,
    cell_width: 8,
    cell_height: 16,
};

/// What a terminal tells its panel.
#[derive(Clone, Debug, PartialEq)]
pub enum TerminalEvent {
    /// The shell named the terminal this.
    Retitled(SharedString),
    /// The program running is at this now.
    Restatused(Status),
    /// The shell exited (or the emulator stopped).
    Exited,
}

/// What a button pressed over the terminal does until it's let go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Held {
    /// The program tracking the mouse is told where it goes.
    Reported,
    /// It selects, as Ghostty does, from the pointer last over `at` with `modifiers` held.
    Selecting {
        at: Point<Pixels>,
        modifiers: Modifiers,
    },
}

/// A terminal entity.
pub struct TerminalView {
    tty: Tty,
    /// How the shell quotes, for dropped paths.
    kind: ShellKind,
    /// The program the shell runs and the name the tab shows in place of its path.
    program: String,
    name: SharedString,
    colors: TerminalColors,
    /// Pixels scrolled that don't make a whole line yet.
    scrolled: Pixels,
    grid: Rc<Cell<Grid>>,
    held: Option<Held>,
    /// The link under the pointer while Ctrl (Cmd on macOS) is held, underlined for a click to open.
    link: Option<Arc<str>>,
    autoscrolling: Option<Task<()>>,
    /// Drawing again once the program's hold on the screen times out, as nothing else may.
    hold_timeout: Option<Task<()>>,
    /// The grid's size as last measured, which the shell follows at most every `RESIZE_EVERY`.
    wanted: Size,
    resizing: Option<Task<()>>,
    focus: FocusHandle,
    /// Whether the terminal is on screen (the panel shows and it is the active tab).
    shown: bool,
    /// Whether the emulator has drawn since the terminal opened.
    drawn_once: bool,
    /// Whether a frame showed text since the terminal opened: the shell's banner or prompt (a pseudoconsole draws
    /// an empty screen first; QA waits for this before typing).
    printed: bool,
    exited: bool,
    /// Text an input method is composing, until it commits it.
    marked: Option<String>,
    _updates: Task<()>,
}

impl EventEmitter<TerminalEvent> for TerminalView {}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl TerminalView {
    /// A view of `tty` running the shell `named`, woken by `updates`, drawn in `colors` (which `tty` was started
    /// with).
    pub(super) fn new(
        tty: Tty,
        mut updates: Updates,
        named: ShellName,
        colors: TerminalColors,
        cx: &mut Context<Self>,
    ) -> Self {
        let size = tty.size();
        Self {
            tty,
            kind: named.kind,
            program: named.program,
            name: named.name,
            colors,
            scrolled: px(0.),
            grid: Rc::new(Cell::new(Grid::opening(size))),
            held: None,
            link: None,
            autoscrolling: None,
            hold_timeout: None,
            wanted: size,
            resizing: None,
            focus: cx.focus_handle(),
            shown: true,
            drawn_once: false,
            printed: false,
            exited: false,
            marked: None,
            // The emulator reads the shell on a thread of its own; this only shows what it last drew.
            _updates: cx.spawn(async move |this, cx| {
                while updates.next().await.is_some() {
                    if this.update(cx, |this, cx| this.updated(cx)).is_err() {
                        return;
                    }
                }
                this.update(cx, |this, cx| {
                    this.exited = true;
                    cx.emit(TerminalEvent::Exited);
                })
                .ok();
            }),
        }
    }

    /// The shell's title for the terminal, with the shell's name in place of its path.
    pub fn title(&self) -> SharedString {
        shown_title(&self.tty.title(), &self.program, &self.name)
    }

    /// What the program running reports.
    pub fn status(&self) -> Status {
        self.tty.status()
    }

    /// Whether a program the shell started runs, whatever it reports.
    pub fn program_running(&self) -> bool {
        self.tty.program_running()
    }

    /// Whether the terminal is on screen.
    pub fn is_shown(&self) -> bool {
        self.shown
    }

    /// Whether the emulator has drawn a frame since the terminal opened.
    pub fn has_drawn(&self) -> bool {
        self.drawn_once
    }

    /// Whether a frame has shown text since the terminal opened (the shell's banner or first prompt).
    pub fn has_printed(&self) -> bool {
        self.printed
    }

    /// Whether the shell exited.
    pub fn has_exited(&self) -> bool {
        self.exited
    }

    /// The grid size the shell was last told.
    pub fn size(&self) -> Size {
        self.tty.size()
    }

    /// The text of the rows the emulator last drew, one line per row (tests, QA).
    pub fn screen_text(&self) -> String {
        let frame = self.tty.frame();
        frame
            .rows
            .iter()
            .map(|row| row.text.trim_end())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Whether this is on screen, for the emulator to draw only then. Out of sight it doesn't have the keys and
    /// drops a press it holds (its pointer listeners go with it).
    pub fn set_shown(&mut self, shown: bool, cx: &mut Context<Self>) {
        self.shown = shown;
        self.tty.set_shown(shown);
        if !shown {
            self.tty.set_focused(false);
            self.held = None;
            self.autoscrolling = None;
        }
        cx.notify();
    }

    /// Selects the whole screen and scrollback.
    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        self.tty.select_all();
        cx.notify();
    }

    /// The selected text, if anything is selected (asks the emulator's thread and waits for the answer).
    pub fn selected_text(&self) -> Option<String> {
        self.tty.selected().filter(|text| !text.is_empty())
    }

    /// Types `text` as it is (a command, a pasted path) and takes the keys.
    pub fn send_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        self.tty.input(text.as_bytes());
        self.typed(cx);
    }

    /// Pastes `text` (bracketed when the program asks) and takes the keys.
    pub fn paste(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        self.tty.paste(text);
        self.typed(cx);
    }

    /// Tells of what the program running is at, if that changed since last told.
    fn restatused(&mut self, cx: &mut Context<Self>) {
        if let Some(status) = self.tty.restatused() {
            cx.emit(TerminalEvent::Restatused(status));
        }
    }

    /// The emulator has more to show.
    fn updated(&mut self, cx: &mut Context<Self>) {
        self.drawn_once = true;
        if !self.printed {
            // Only until the first text: the rows are shared with the emulator's frame, nothing is copied.
            self.printed = self
                .tty
                .frame()
                .rows
                .iter()
                .any(|row| !row.text.trim().is_empty());
        }
        if let Some(title) = self.tty.retitled() {
            cx.emit(TerminalEvent::Retitled(shown_title(
                &title,
                &self.program,
                &self.name,
            )));
        }
        self.restatused(cx);
        // What a program in it copies, as OSC 52 asks.
        if let Some(text) = self.tty.copied() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
        // A moment past it, as a redraw before it changes nothing and so draws nothing to wake this again.
        self.hold_timeout = self.tty.hold_left().map(|left| {
            cx.spawn(async move |this, cx| {
                let past = left + Duration::from_millis(1);
                cx.background_executor().timer(past).await;
                this.update(cx, |this, _| this.tty.redraw()).ok();
            })
        });
        cx.notify();
    }

    /// Resizes the shell to `size` now, or once `RESIZE_EVERY` has passed since it last was, to the size wanted then.
    fn resize(&mut self, size: Size, cx: &mut Context<Self>) {
        self.wanted = size;
        if self.resizing.is_some() {
            return;
        }
        self.tty.resize(size);
        cx.notify();
        self.resizing = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(RESIZE_EVERY).await;
            this.update(cx, |this, cx| {
                this.resizing = None;
                if this.wanted != this.tty.size() {
                    this.resize(this.wanted, cx);
                }
            })
            .ok();
        }));
    }

    fn typed(&mut self, cx: &mut Context<Self>) {
        self.scrolled = px(0.);
        cx.notify();
    }

    /// The theme's colours, applied when they changed (theme switch).
    fn sync_colors(&mut self, cx: &App) {
        let theme = cx.theme_g();
        let colors = TerminalColors::of(&theme.tokens, &theme.lanes);
        if colors != self.colors {
            self.tty
                .set_colors(&colors.colors, colors.background, colors.foreground);
            self.colors = colors;
        }
    }

    /// `position` in the window on the terminal's surface.
    fn surface(&self, position: Point<Pixels>) -> mouse::Position {
        self.grid.get().surface(position, self.tty.size())
    }

    fn copy_selection(&mut self, cx: &mut Context<Self>) -> bool {
        match self.selected_text() {
            Some(text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                true
            }
            None => false,
        }
    }

    fn paste_clipboard(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.tty.paste(&text);
        }
        self.typed(cx);
    }

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        match keys::route(&event.keystroke, event.prefer_character_input) {
            // Left unhandled, so the platform hands the text to the input handler (on Windows it does anyway).
            KeyRoute::Text | KeyRoute::App => return,
            KeyRoute::Copy => {
                self.copy_selection(cx);
            }
            KeyRoute::Paste => self.paste_clipboard(cx),
            KeyRoute::CopyOrInterrupt => {
                if !self.copy_selection(cx) {
                    self.tty.input(b"\x03");
                    self.typed(cx);
                }
            }
            KeyRoute::SelectAll => self.select_all(cx),
            KeyRoute::Encode {
                key,
                mods,
                text,
                unshifted,
            } => {
                self.tty.key(key, mods, text, unshifted);
                self.typed(cx);
            }
        }
        cx.stop_propagation();
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        if event.button == MouseButton::Left
            && keys::follows_links(event.modifiers)
            && let Some(link) = self.link_at(event.position)
        {
            cx.open_url(&link);
        } else if self.reports(event.modifiers) {
            self.report(
                mouse::Action::Press,
                keys::button(event.button),
                event.position,
                event.modifiers,
            );
            self.held = Some(Held::Reported);
        } else if event.button == MouseButton::Left {
            let at = self.surface(event.position);
            self.tty.select_press(at, event.click_count > 1);
            self.held = Some(Held::Selecting {
                at: event.position,
                modifiers: event.modifiers,
            });
            cx.notify();
        } else if event.button == MouseButton::Right {
            self.open_menu(event.position, window, cx);
        }
        cx.stop_propagation();
    }

    /// The context menu: Copy (with a selection), Paste, Select all.
    fn open_menu(&mut self, at: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let has_selection = self.selected_text().is_some();
        let this = cx.entity().downgrade();
        let (copy, paste, all) = (this.clone(), this.clone(), this);
        let items = vec![
            MenuItem::action("terminal-copy", "Copy", move |_, cx| {
                copy.update(cx, |view, cx| {
                    view.copy_selection(cx);
                })
                .ok();
            })
            .with_icon(AppIcon::Copy)
            .enabled(has_selection),
            MenuItem::action("terminal-paste", "Paste", move |window, cx| {
                paste
                    .update(cx, |view, cx| {
                        window.focus(&view.focus, cx);
                        view.paste_clipboard(cx);
                    })
                    .ok();
            })
            .with_icon(AppIcon::Paste),
            MenuItem::action("terminal-select-all", "Select all", move |_, cx| {
                all.update(cx, |view, cx| view.select_all(cx)).ok();
            })
            .with_icon(AppIcon::Done),
        ];
        menus::open_context_menu(window, cx, at, items);
    }

    /// While a button pressed over the terminal is held it follows the pointer anywhere, first in line so nothing
    /// under the pointer reacts; otherwise only over the terminal.
    fn mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        phase: DispatchPhase,
        hovered: bool,
        cx: &mut Context<Self>,
    ) {
        match (self.held, phase) {
            (Some(Held::Reported), DispatchPhase::Capture) => {
                let button = event.pressed_button.and_then(keys::button);
                self.report(
                    mouse::Action::Motion,
                    button,
                    event.position,
                    event.modifiers,
                );
                cx.stop_propagation();
            }
            (Some(Held::Selecting { .. }), DispatchPhase::Capture) => {
                self.held = Some(Held::Selecting {
                    at: event.position,
                    modifiers: event.modifiers,
                });
                let at = self.surface(event.position);
                self.tty.select_drag(at, keys::rectangle(event.modifiers));
                self.autoscroll(event.position, cx);
                cx.notify();
                cx.stop_propagation();
            }
            (None, DispatchPhase::Bubble) => {
                if hovered && self.reports(event.modifiers) {
                    self.report(mouse::Action::Motion, None, event.position, event.modifiers);
                }
                self.hover(hovered.then_some(event.position), event.modifiers, cx);
            }
            _ => {}
        }
    }

    /// Holding Ctrl down or letting it go over a link underlines it or stops.
    fn modifiers_changed(
        &mut self,
        event: &ModifiersChangedEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.hover(Some(window.mouse_position()), event.modifiers, cx);
    }

    /// Underlines the link under the pointer, over the terminal at `position` in the window, while `modifiers`
    /// follow links.
    fn hover(
        &mut self,
        position: Option<Point<Pixels>>,
        modifiers: Modifiers,
        cx: &mut Context<Self>,
    ) {
        let link = position
            .filter(|_| keys::follows_links(modifiers))
            .and_then(|position| self.link_at(position));
        if link != self.link {
            self.link = link;
            cx.notify();
        }
    }

    fn link_at(&self, position: Point<Pixels>) -> Option<Arc<str>> {
        grid::link_at(&self.tty.frame().rows, self.grid.get(), position)
    }

    fn mouse_up(&mut self, event: &MouseUpEvent, phase: DispatchPhase) {
        if phase != DispatchPhase::Capture {
            return;
        }
        match self.held.take() {
            Some(Held::Reported) => {
                self.report(
                    mouse::Action::Release,
                    keys::button(event.button),
                    event.position,
                    event.modifiers,
                );
            }
            Some(Held::Selecting { .. }) => {
                let at = self.surface(event.position);
                self.tty.select_release(at);
                self.autoscrolling = None;
            }
            None => {}
        }
    }

    /// Ticks while the selection is dragged past the top or bottom, about as often as Ghostty's do.
    fn autoscroll(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        if !self.past_edge(position) {
            self.autoscrolling = None;
            return;
        }
        if self.autoscrolling.is_some() {
            return;
        }
        self.autoscrolling = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(AUTOSCROLL_EVERY).await;
                let ticked = this.update(cx, |this, cx| {
                    let Some(Held::Selecting { at, modifiers }) = this.held else {
                        return false;
                    };
                    if !this.past_edge(at) {
                        return false;
                    }
                    let surface = this.surface(at);
                    cx.notify();
                    this.tty
                        .select_autoscroll(surface, keys::rectangle(modifiers));
                    true
                });
                if !ticked.unwrap_or(false) {
                    break;
                }
            }
            this.update(cx, |this, _| this.autoscrolling = None).ok();
        }));
    }

    /// Whether `position` in the window is above the grid's top or below its bottom.
    fn past_edge(&self, position: Point<Pixels>) -> bool {
        let grid = self.grid.get();
        let y = position.y - grid.origin.y;
        y < px(0.) || y >= grid.cell.height * f32::from(self.tty.size().rows)
    }

    /// Whether the program tracking the mouse is told of it. Shift keeps it for selecting, as Ghostty's
    /// `mouse-shift-capture` does by default.
    fn reports(&self, modifiers: Modifiers) -> bool {
        !modifiers.shift && self.tty.tracks_mouse()
    }

    fn report(
        &mut self,
        action: mouse::Action,
        button: Option<mouse::Button>,
        position: Point<Pixels>,
        modifiers: Modifiers,
    ) {
        let at = self.surface(position);
        self.tty.mouse(action, button, at, keys::mods(modifiers));
    }

    /// Files dropped on the terminal are pasted as their paths, quoted for the shell.
    fn dropped(&mut self, paths: &ExternalPaths, window: &mut Window, cx: &mut Context<Self>) {
        let words: Vec<String> = paths
            .paths()
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect();
        let text = super::shell::quoted(self.kind, words);
        self.paste(&text, window, cx);
    }

    /// The wheel scrolls the scrollback (or is reported to a program tracking the mouse), whole lines at a time.
    fn scroll(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let line = px(f32::from(self.tty.size().cell_height.max(1)));
        self.scrolled += event.delta.pixel_delta(line).y;
        let lines = (self.scrolled / line).trunc();
        if lines != 0. {
            self.scrolled -= line * lines;
            let at = self.surface(event.position);
            self.tty.scroll(lines, at, keys::mods(event.modifiers));
            cx.notify();
        }
        cx.stop_propagation();
    }

    /// The cursor's cell in the window, for an input method's candidate window.
    fn cursor_bounds(&self) -> Option<Bounds<Pixels>> {
        let cursor = self.tty.frame().cursor.clone()?;
        let grid = self.grid.get();
        Some(Bounds::new(
            grid.cell_origin(cursor.x, usize::from(cursor.y)),
            grid.cell,
        ))
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_colors(cx);
        // Read in place: this renders once per batch of output, so it doesn't clone the settings.
        let text_size = cx
            .try_global::<crate::settings::SettingsGlobal>()
            .map_or(DEFAULT_TEXT_SIZE, |settings| settings.0.terminal_font_size)
            .clamp(TEXT_SIZE_RANGE.0, TEXT_SIZE_RANGE.1);
        let font_size = dpx(text_size, window);
        let font = gpui_kit::font(FONT_MONO);
        let metrics = CellMetrics::measure(&font, font_size, window);
        let (cell_width, cell_height) = (metrics.cell.width, metrics.cell.height);
        let focused = self.focus.is_focused(window);
        // GPUI draws the window again as focus moves and as it comes in front or goes behind.
        self.tty.set_focused(focused && window.is_window_active());
        // Given the keys, a result it showed is no news.
        self.restatused(cx);
        let pointer_style = match (&self.link, self.tty.tracks_mouse()) {
            (Some(_), _) => CursorStyle::PointingHand,
            (None, true) => CursorStyle::Arrow,
            (None, false) => CursorStyle::IBeam,
        };
        let link = self.link.clone();
        let frame = self.tty.frame();
        let rows = frame.clone();
        let this = cx.entity().downgrade();
        let input_target = cx.entity();
        let focus = self.focus.clone();
        let measured = Rc::new(Cell::new(self.wanted));
        let drawn = self.grid.clone();
        let pointer = this.clone();
        let background = hsla(frame.background);
        let foreground = hsla(frame.foreground);
        let cursor_color = hsla(frame.cursor_color);
        let scale = window.scale_factor();
        // A bar or underline is a device pixel thick, as Ghostty's.
        let thickness = px(1.) / scale;
        let cursor = frame.cursor.clone().map(|Cursor { x, y, text, style }| {
            let at = div()
                .absolute()
                .left(cell_width * f32::from(x))
                .top(cell_height * f32::from(y))
                .w(cell_width)
                .h(cell_height);
            // In the shape the program asks for, but hollow while unfocused whatever it is, as in Ghostty.
            match style {
                _ if !focused => at.border_1().border_color(cursor_color),
                CursorVisualStyle::BlockHollow => at.border_1().border_color(cursor_color),
                CursorVisualStyle::Bar => at.w(thickness).bg(cursor_color),
                CursorVisualStyle::Underline => at
                    .top(
                        cell_height * f32::from(y)
                            + gpui_kit::underline_y_offset(
                                cell_height,
                                metrics.ascent,
                                metrics.descent,
                            ),
                    )
                    .h(thickness)
                    .bg(cursor_color),
                _ => at.bg(cursor_color).text_color(background).child(text),
            }
        });
        let mut cursor = div().size_full().children(cursor).into_any_element();
        let bold = FontWeight::SEMIBOLD;
        let paint_font = font.clone();
        let grid_element = canvas(
            move |bounds, window, cx| {
                let size = grid::grid_size(bounds.size, metrics.cell);
                let placed = Grid::at(bounds, metrics.cell, scale);
                drawn.set(placed);
                if size != measured.get() {
                    measured.set(size);
                    let this = this.clone();
                    window.defer(cx, move |_, cx| {
                        this.update(cx, |this, cx| this.resize(size, cx)).ok();
                    });
                }
                let space = bounds.size.map(AvailableSpace::Definite);
                cursor.prepaint_as_root(placed.origin, space, window, cx);
                let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
                (hitbox, placed, cursor)
            },
            move |bounds, (hitbox, placed, mut cursor), window, cx| {
                let style = PaintStyle {
                    regular: &paint_font,
                    bold,
                    font_size,
                    metrics,
                    link: link.as_deref(),
                };
                grid::paint_rows(&rows.rows, placed, &style, window, cx);
                cursor.paint(window, cx);
                window.handle_input(&focus, ElementInputHandler::new(bounds, input_target), cx);
                window.on_mouse_event({
                    let this = pointer.clone();
                    move |event: &MouseMoveEvent, phase, window, cx| {
                        let hovered = hitbox.is_hovered(window);
                        this.update(cx, |this, cx| this.mouse_move(event, phase, hovered, cx))
                            .ok();
                    }
                });
                window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                    pointer
                        .update(cx, |this, _| this.mouse_up(event, phase))
                        .ok();
                });
            },
        )
        .absolute()
        .size_full();
        div()
            .id(ids::VIEW)
            .key_context(TERMINAL_CONTEXT)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key_down))
            .on_any_mouse_down(cx.listener(Self::mouse_down))
            .on_modifiers_changed(cx.listener(Self::modifiers_changed))
            .on_drop(cx.listener(Self::dropped))
            .on_scroll_wheel(cx.listener(Self::scroll))
            .cursor(pointer_style)
            .size_full()
            // Ghostty's default window padding.
            .p(px(2.))
            .bg(background)
            .text_color(foreground)
            .font_family(font.family)
            .text_size(font_size)
            .line_height(cell_height)
            .child(
                div()
                    .relative()
                    .size_full()
                    .overflow_hidden()
                    .child(grid_element),
            )
            .test_support()
    }
}

impl EntityInputHandler for TerminalView {
    fn text_for_range(
        &mut self,
        _range: Range<usize>,
        _adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: 0..0,
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked
            .as_ref()
            .map(|text| 0..text.encode_utf16().count())
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.marked = None;
    }

    fn paste(&mut self, item: ClipboardItem, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = item.text() {
            self.tty.paste(&text);
            self.typed(cx);
        }
    }

    fn replace_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked = None;
        if !text.is_empty() {
            self.tty.input(text.as_bytes());
            self.typed(cx);
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<Range<usize>>,
        new_text: &str,
        _new_selected_range: Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        self.marked = (!new_text.is_empty()).then(|| new_text.to_owned());
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        _element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        self.cursor_bounds()
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

/// What the shell titled the terminal, or the shell's `name` while it set no title. A pseudoconsole titles it with the
/// `program`'s path (cmd.exe adds " - <command>" while one runs; an elevated console puts a localized
/// "Administrator: " before it), so that path, with whatever prefix ends in ": ", reads as the `name`.
pub(super) fn shown_title(title: &str, program: &str, name: &SharedString) -> SharedString {
    let title = title.trim();
    if title.is_empty() {
        return if name.is_empty() {
            "Terminal".into()
        } else {
            name.clone()
        };
    }
    if program.is_empty() || name.is_empty() {
        return title.to_owned().into();
    }
    // The path starts the title or follows a prefix's ": " (a path's own colon is followed by a separator).
    let starts =
        std::iter::once(0).chain(title.match_indices(": ").map(|(at, sep)| at + sep.len()));
    let rest = starts
        .filter(|&start| {
            title
                .get(start..start + program.len())
                .is_some_and(|path| path.eq_ignore_ascii_case(program))
        })
        .filter_map(|start| title.get(start + program.len()..))
        .find(|rest| rest.is_empty() || rest.starts_with(' '));
    match rest {
        Some(rest) => format!("{name}{rest}").into(),
        None => title.to_owned().into(),
    }
}
