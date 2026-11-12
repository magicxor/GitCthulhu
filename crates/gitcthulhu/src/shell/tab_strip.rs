//! The repository tab strip (owned by G1 after G0b; 07 §2). The placeholder lists the tabs as plain 36 dp cells
//! that select on click; G1 implements the real strip (close buttons, drag reorder, "+").

use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, WeakEntity, Window, div,
};

use crate::app_view::AppView;
use crate::ids;
use crate::metrics::{self, dp};
use crate::theme::{ActiveTokens as _, TextStyleExt as _};
use crate::widgets::observed;

/// The tab strip view of the window.
pub struct TabStripView {
    app: WeakEntity<AppView>,
}

impl TabStripView {
    /// A strip that shows (and drives) `app`'s tabs.
    pub fn new(app: WeakEntity<AppView>, _window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self { app }
    }

    /// The window content this strip belongs to.
    pub fn app(&self) -> &WeakEntity<AppView> {
        &self.app
    }
}

impl Render for TabStripView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = *cx.tokens();
        let tabs: Vec<(usize, SharedString, bool)> = match self.app.upgrade() {
            Some(app) => {
                let app = app.read(cx);
                (0..app.tabs().len())
                    .map(|ix| (ix, app.tab_title(ix, cx), ix == app.active()))
                    .collect()
            }
            None => Vec::new(),
        };
        let cells: Vec<_> = tabs
            .into_iter()
            .map(|(ix, title, selected)| {
                let app = self.app.clone();
                div()
                    .id(("tab", ix))
                    .flex_none()
                    .min_w(dp(metrics::TAB_MIN_W))
                    .h_full()
                    .px(dp(metrics::TAB_TITLE_PAD_START))
                    .flex()
                    .items_center()
                    .text_body2()
                    .medium()
                    .bg(if selected {
                        tokens.surface
                    } else {
                        tokens.background
                    })
                    .child(title)
                    .on_click(move |_, window, cx| {
                        app.update(cx, |app, cx| app.select_tab(ix, window, cx))
                            .ok();
                    })
            })
            .collect();
        observed(ids::TAB_STRIP)
            .flex_none()
            .w_full()
            .h(dp(metrics::TAB_STRIP_H))
            .flex()
            .flex_row()
            .bg(tokens.background)
            .children(cells)
    }
}
