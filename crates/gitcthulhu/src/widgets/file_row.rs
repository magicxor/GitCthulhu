//! File rows (Gitnuro 08 §4.4): the row of the status and commit-file lists and the directory row of tree mode.
//!
//! A row is `file_row` dp high (38 spaced, 34 compact), shows a 16 dp status icon, the dimmed parent directory
//! (which ellipsizes first) and the file name (clipped, never ellipsized). The trailing action ("Stage" /
//! "Unstage") appears only while the row is hovered.

use std::rc::Rc;

use crate::backend::ByteSlice as _;
use crate::backend::ChangeKind;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ClickEvent, ElementId, Hsla, InteractiveElement as _, IntoElement,
    MouseButton, MouseDownEvent, ParentElement as _, RenderOnce, SharedString,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, div,
};

use crate::icons::{AppIcon, icon};
use crate::metrics::{self, dp};
use crate::theme::{ActiveTokens as _, TextStyleExt as _, Tokens};
use crate::widgets::with_layer;

/// Icon and colour of a change kind (08 §4.4 table). Untracked files show as added.
pub fn status_icon(kind: ChangeKind, tokens: &Tokens) -> (AppIcon, Hsla) {
    match kind {
        ChangeKind::Added | ChangeKind::Untracked | ChangeKind::IntentToAdd => {
            (AppIcon::Add, tokens.add_file)
        }
        ChangeKind::Modified | ChangeKind::TypeChanged => (AppIcon::Edit, tokens.modified_file),
        ChangeKind::Deleted => (AppIcon::Delete, tokens.deleted_file),
        ChangeKind::Conflicted => (AppIcon::Warning, tokens.conflicting_file),
        ChangeKind::Renamed => (AppIcon::Refresh, tokens.modified_file),
    }
}

/// Splits a repository-relative path into `(parent directory without trailing slash, file name)` at the last `/`,
/// converting lossily at the UI boundary only.
pub fn split_path(path: &[u8]) -> (String, String) {
    match path.rfind_byte(b'/') {
        Some(at) => (
            path[..at].to_str_lossy().into_owned(),
            path[at + 1..].to_str_lossy().into_owned(),
        ),
        None => (String::new(), path.to_str_lossy().into_owned()),
    }
}

/// The background of a hovered file row (08 §4.4): the state layer over the row's own background, so a selected
/// row stays selected under the pointer.
pub(crate) fn hover_background(selected: bool, tokens: &Tokens) -> Hsla {
    if selected {
        with_layer(tokens.background_selected, tokens.hover_layer())
    } else {
        tokens.hover_layer()
    }
}

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;
type MenuHandler = Rc<dyn Fn(&MouseDownEvent, &mut Window, &mut App)>;

/// One file row.
#[derive(IntoElement)]
pub struct FileRow {
    id: ElementId,
    path: Vec<u8>,
    kind: ChangeKind,
    selected: bool,
    depth: u32,
    tree_mode: bool,
    trailing: Option<AnyElement>,
    on_click: Option<ClickHandler>,
    on_context_menu: Option<MenuHandler>,
}

impl FileRow {
    /// A row for `path` (byte-preserving). `id` must be unique in the frame.
    pub fn new(id: impl Into<ElementId>, path: impl Into<Vec<u8>>, kind: ChangeKind) -> Self {
        Self {
            id: id.into(),
            path: path.into(),
            kind,
            selected: false,
            depth: 0,
            tree_mode: false,
            trailing: None,
            on_click: None,
            on_context_menu: None,
        }
    }

    /// Highlights the row as selected.
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Tree mode: indents by 12 dp per `depth` and shows only the file name.
    pub fn tree(mut self, depth: u32) -> Self {
        self.tree_mode = true;
        self.depth = depth;
        self
    }

    /// The action shown at the right end while the row is hovered.
    pub fn trailing(mut self, action: impl IntoElement) -> Self {
        self.trailing = Some(action.into_any_element());
        self
    }

    /// Click handler; the event carries modifiers and the click count.
    pub fn on_click(mut self, f: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(f));
        self
    }

    /// Right-button press handler (opens the context menu at the pointer).
    pub fn on_context_menu(
        mut self,
        f: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_context_menu = Some(Rc::new(f));
        self
    }
}

impl RenderOnce for FileRow {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let tokens = *cx.tokens();
        let height = metrics::line_heights(cx).file_row;
        let (parent, name) = split_path(&self.path);
        let (role, color) = status_icon(self.kind, &tokens);
        let on_click = self.on_click;
        let on_context_menu = self.on_context_menu;
        let show_parent = !self.tree_mode && !parent.is_empty();
        let selected = self.selected;
        div()
            .id(self.id)
            .group("file-row")
            .relative()
            .w_full()
            .h(dp(height))
            .flex_none()
            .cursor_pointer()
            .when(self.selected, |r| r.bg(tokens.background_selected))
            .hover(move |s| s.bg(hover_background(selected, &tokens)))
            .when_some(on_click, |r, f| {
                r.on_click(move |ev, window, cx| f(ev, window, cx))
            })
            .when_some(on_context_menu, |r, f| {
                r.on_mouse_down(MouseButton::Right, move |ev, window, cx| f(ev, window, cx))
            })
            .child(
                div()
                    .size_full()
                    .flex()
                    .flex_row()
                    .items_center()
                    .pl(dp(12. * self.depth as f32))
                    .child(
                        div()
                            .flex_none()
                            .px(dp(8.))
                            .child(icon(role, metrics::ICON_SM, color)),
                    )
                    .when(show_parent, |r| {
                        r.child(
                            div()
                                .min_w_0()
                                .flex_shrink(1.)
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .text_body2()
                                .text_color(tokens.on_background_secondary)
                                .child(SharedString::from(parent)),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_body2()
                                .text_color(tokens.on_background_secondary)
                                .child("/"),
                        )
                    })
                    .child(
                        div()
                            .flex_none()
                            .pr(dp(16.))
                            .whitespace_nowrap()
                            .text_body2()
                            .text_color(tokens.on_background)
                            .child(SharedString::from(name)),
                    ),
            )
            .children(self.trailing.map(|action| {
                div()
                    .absolute()
                    .right_0()
                    .top_0()
                    .h_full()
                    .px(dp(16.))
                    .flex()
                    .items_center()
                    .invisible()
                    .group_hover("file-row", |s| s.visible())
                    .child(action)
            }))
            .test_support()
    }
}

/// A directory row of tree mode: folder icon (open or closed), never selected, click toggles collapse.
#[derive(IntoElement)]
pub struct DirectoryRow {
    id: ElementId,
    name: SharedString,
    depth: u32,
    expanded: bool,
    on_toggle: Option<Rc<dyn Fn(&mut Window, &mut App)>>,
}

impl DirectoryRow {
    /// A directory row. `id` must be unique in the frame.
    pub fn new(
        id: impl Into<ElementId>,
        name: impl Into<SharedString>,
        depth: u32,
        expanded: bool,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            depth,
            expanded,
            on_toggle: None,
        }
    }

    /// Sets the handler that toggles the collapsed state.
    pub fn on_toggle(mut self, f: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_toggle = Some(Rc::new(f));
        self
    }
}

impl RenderOnce for DirectoryRow {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let tokens = *cx.tokens();
        let height = metrics::line_heights(cx).file_row;
        let on_toggle = self.on_toggle;
        div()
            .id(self.id)
            .w_full()
            .h(dp(height))
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .pl(dp(12. * self.depth as f32))
            .cursor_pointer()
            .hover(move |s| s.bg(tokens.hover_layer()))
            .when_some(on_toggle, |r, f| {
                r.on_click(move |_, window, cx| f(window, cx))
            })
            .child(div().flex_none().px(dp(8.)).child(icon(
                if self.expanded {
                    AppIcon::FolderOpen
                } else {
                    AppIcon::Folder
                },
                metrics::ICON_SM,
                tokens.on_background,
            )))
            .child(
                div()
                    .flex_none()
                    .pr(dp(16.))
                    .whitespace_nowrap()
                    .text_body2()
                    .text_color(tokens.on_background)
                    .child(self.name),
            )
            .test_support()
    }
}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;

    use super::*;

    #[test]
    fn split_path_separates_parent_and_name() {
        assert_eq!(
            split_path(b"src/widgets/file_row.rs"),
            ("src/widgets".into(), "file_row.rs".into())
        );
        assert_eq!(
            split_path(b"README.md"),
            (String::new(), "README.md".into())
        );
    }

    #[test]
    fn status_icons_follow_the_table() {
        let tokens = crate::theme::builtin::dark();
        assert_eq!(
            status_icon(ChangeKind::Added, &tokens),
            (AppIcon::Add, tokens.add_file)
        );
        assert_eq!(
            status_icon(ChangeKind::Deleted, &tokens),
            (AppIcon::Delete, tokens.deleted_file)
        );
        assert_eq!(
            status_icon(ChangeKind::Conflicted, &tokens),
            (AppIcon::Warning, tokens.conflicting_file)
        );
        assert_eq!(
            status_icon(ChangeKind::Renamed, &tokens),
            (AppIcon::Refresh, tokens.modified_file)
        );
    }
}
