//! The embedded-terminal settings rows (D0.4; spec 21 §5.4, 18 §5.5); they replace the Terminal page body when
//! `terminal::settings_available` is true: Shell (Automatic, the shells found on this computer, Custom), Custom shell
//! command (enabled iff Custom) and Font size. Shell changes apply to terminals opened afterwards; the font size
//! applies at once.

use gpui_kit::{AnyElement, App, IntoElement, Window};

/// The page content.
pub fn page(_window: &mut Window, _cx: &mut App) -> AnyElement {
    #[cfg(feature = "terminal")]
    {
        rows::TerminalPage.into_any_element()
    }
    #[cfg(not(feature = "terminal"))]
    {
        gpui_kit::div().into_any_element()
    }
}

#[cfg(feature = "terminal")]
mod rows {
    use gpui_kit::prelude::FluentBuilder as _;
    use gpui_kit::{
        AnyElement, App, AppContext as _, BoxShadow, Context, Entity, InteractiveElement as _,
        IntoElement, MouseButton, ParentElement as _, RenderOnce, SharedString,
        StatefulInteractiveElement as _, Styled as _, Subscription, TestSupportExt as _, Window,
        deferred, div, point, px,
    };

    use super::super::inputs::{NumberEvent, NumberField};
    use super::super::{SettingRow, ids};
    use crate::icons::{AppIcon, icon};
    use crate::metrics::{self, dp, settings as sm};
    use crate::settings::{self, TerminalShell};
    use crate::terminal::shell;
    use crate::theme::{ActiveTokens as _, TextStyleExt as _};
    use crate::widgets::field::{FieldEvent, OutlinedField};

    /// Row keys (element ids `settings-row-<key>`, `settings-select-<key>`, `settings-field-<key>`).
    pub const SHELL: &str = "terminal-shell";
    pub const CUSTOM: &str = "terminal-custom-shell";
    pub const FONT_SIZE: &str = "terminal-font-size";

    /// The smallest and largest font size the field takes.
    const FONT_SIZE_RANGE: std::ops::RangeInclusive<u64> = 6..=48;

    /// The page's inputs, kept while the page shows.
    struct PageState {
        custom: Entity<OutlinedField>,
        font_size: Entity<NumberField>,
        shell_open: bool,
        _subscriptions: Vec<Subscription>,
    }

    impl PageState {
        fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
            let current = settings::get_or_default(cx);
            let custom = cx.new(|cx| OutlinedField::new(ids::field(CUSTOM), window, cx));
            let font_size = cx.new(|cx| NumberField::new(ids::field(FONT_SIZE), window, cx));
            let custom_text = match &current.terminal_shell {
                TerminalShell::Custom(line) => line.clone(),
                _ => String::new(),
            };
            custom.update(cx, |field, cx| {
                field.set_text(&custom_text, window, cx);
                field.set_disabled(
                    !matches!(current.terminal_shell, TerminalShell::Custom(_)),
                    cx,
                );
            });
            font_size.update(cx, |field, cx| {
                field.set_value(current.terminal_font_size.round() as u64, window, cx)
            });
            let subscriptions = vec![
                cx.subscribe(&custom, |_, field, _: &FieldEvent, cx| {
                    let line = field.read(cx).text(cx).to_string();
                    if matches!(
                        settings::get_or_default(cx).terminal_shell,
                        TerminalShell::Custom(_)
                    ) {
                        settings::update(cx, |s| s.terminal_shell = TerminalShell::Custom(line));
                    }
                }),
                cx.subscribe(
                    &font_size,
                    |_, field, event: &NumberEvent, cx| match event {
                        NumberEvent::Changed(Some(size)) if FONT_SIZE_RANGE.contains(size) => {
                            let size = *size as f32;
                            settings::update(cx, |s| s.terminal_font_size = size);
                        }
                        NumberEvent::Changed(Some(_)) => field.update(cx, |f, cx| f.reject(cx)),
                        NumberEvent::Changed(None) => {}
                    },
                ),
            ];
            Self {
                custom,
                font_size,
                shell_open: false,
                _subscriptions: subscriptions,
            }
        }

        /// Stores `choice`; Custom takes the command line typed so far.
        fn choose(&mut self, choice: TerminalShell, cx: &mut Context<Self>) {
            let choice = match choice {
                TerminalShell::Custom(_) => {
                    TerminalShell::Custom(self.custom.read(cx).text(cx).to_string())
                }
                other => other,
            };
            let custom = matches!(choice, TerminalShell::Custom(_));
            settings::update(cx, |s| s.terminal_shell = choice);
            self.custom
                .update(cx, |field, cx| field.set_disabled(!custom, cx));
            self.shell_open = false;
            cx.notify();
        }
    }

    /// The options of the Shell dropdown: Automatic, the shells found here, Custom.
    pub fn shell_options() -> Vec<TerminalShell> {
        std::iter::once(TerminalShell::Auto)
            .chain(
                shell::detected()
                    .iter()
                    .map(|found| found.choice.clone())
                    .filter(|choice| *choice != TerminalShell::Auto),
            )
            .chain(std::iter::once(TerminalShell::Custom(String::new())))
            .collect()
    }

    #[derive(IntoElement)]
    pub struct TerminalPage;

    impl RenderOnce for TerminalPage {
        fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
            let state = window.use_keyed_state("terminal-settings", cx, PageState::new);
            let current = settings::get_or_default(cx);
            let custom = matches!(current.terminal_shell, TerminalShell::Custom(_));
            let automatic = if cfg!(windows) {
                "Automatic starts PowerShell 7, else Windows PowerShell, else Command Prompt."
            } else {
                "Automatic starts your login shell."
            };
            let (custom_field, font_field) = {
                let state = state.read(cx);
                (state.custom.clone(), state.font_size.clone())
            };
            div()
                .w_full()
                .flex()
                .flex_col()
                .child(
                    SettingRow::new(SHELL, "Shell")
                        .subtitle(format!(
                            "The shell new terminals start in the repository's folder. {automatic}"
                        ))
                        .control(shell_select(&state, &current.terminal_shell, cx)),
                )
                .child(
                    SettingRow::new(CUSTOM, "Custom shell command")
                        .subtitle("The command line of your own shell, for example: nu -l")
                        .disabled(!custom)
                        .control(div().w(dp(sm::TEXT_FIELD_W)).child(custom_field)),
                )
                .child(
                    SettingRow::new(FONT_SIZE, "Font size")
                        .subtitle("Text size of the terminal, from 6 to 48")
                        .control(font_field),
                )
        }
    }

    /// The Shell dropdown (09 §0.3 `DropDownMenuText`, as the dialog's other dropdowns): a 180 dp box showing the
    /// choice; its options pop up below it, drawn inside the dialog.
    fn shell_select(
        state: &Entity<PageState>,
        current: &TerminalShell,
        cx: &mut App,
    ) -> AnyElement {
        let tokens = *cx.tokens();
        let open = state.read(cx).shell_open;
        let toggle = state.clone();
        let trigger = div()
            .id(ids::select(SHELL))
            .w_full()
            .flex()
            .flex_row()
            .items_center()
            .py(dp(6.))
            .px(dp(10.))
            .rounded(dp(metrics::RADIUS))
            .border(dp(2.))
            .border_color(tokens.dialog_border())
            .bg(tokens.background)
            .cursor_pointer()
            .child(
                div()
                    .flex_1()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_body2()
                    .text_color(tokens.on_background)
                    .child(shell::label(current)),
            )
            .child(icon(AppIcon::ExpandMore, 24., tokens.on_background))
            .on_click(move |_, _, cx| {
                toggle.update(cx, |state, cx| {
                    state.shell_open = !state.shell_open;
                    cx.notify();
                })
            })
            .test_support();
        let popup = open.then(|| {
            let rows: Vec<AnyElement> = shell_options()
                .into_iter()
                .enumerate()
                .map(|(index, choice)| {
                    let state = state.clone();
                    let label: SharedString = shell::label(&choice).into();
                    div()
                        .id(ids::select_option(SHELL, index))
                        .w_full()
                        .px(dp(12.))
                        .py(dp(metrics::MENU_ITEM_PAD))
                        .text_body2()
                        .text_color(tokens.on_background)
                        .hover(move |s| s.bg(tokens.hover_layer()))
                        .active(move |s| s.bg(tokens.press_layer()))
                        .on_click(move |_, _, cx| {
                            let choice = choice.clone();
                            state.update(cx, |state, cx| state.choose(choice, cx));
                        })
                        .child(label)
                        .test_support()
                        .into_any_element()
                })
                .collect();
            let close = state.clone();
            deferred(
                div()
                    .id(ids::select_popup(SHELL))
                    .absolute()
                    .top(dp(40.))
                    .left_0()
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
                    .on_mouse_down_out(move |_, _, cx| {
                        close.update(cx, |state, cx| {
                            state.shell_open = false;
                            cx.notify();
                        })
                    })
                    // The options lie over the rows below (the Custom field, Font size): they must take the
                    // pointer, or a press on an option reaches the field under it instead.
                    .occlude()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .children(rows)
                    .test_support(),
            )
            .with_priority(1)
        });
        div()
            .relative()
            .flex_none()
            .w(dp(sm::DROPDOWN_W))
            .child(trigger)
            .children(popup)
            .into_any_element()
    }
}

#[cfg(all(test, feature = "terminal"))]
pub use rows::{CUSTOM, FONT_SIZE, SHELL, shell_options};
