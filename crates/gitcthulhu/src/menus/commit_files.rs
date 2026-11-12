//! The committed-file menu (owned by G6 after G0b; 09 §4.8): the entry point to Blame and File history from the
//! commit changes list. Gitnuro's guard (`changeType != ADD || changeType != DELETE`) is always true, so all three
//! items are always shown.

use gpui_kit::{App, Window};

use super::{MenuItem, MenuSlot, MenuTarget, extend};
use crate::backend::{BlameRev, ObjectId, RelaPath};
use crate::commit_changes::platform;
use crate::icons::AppIcon;
use crate::tab::TabCtx;

/// Menu item ids (the menu host registers them as `menu-item-<id>`).
pub mod ids {
    /// "Blame file".
    pub const BLAME: &str = "commit-file-blame";
    /// "File history".
    pub const HISTORY: &str = "commit-file-history";
    /// "Open file in folder".
    pub const OPEN_IN_FOLDER: &str = "commit-file-open-in-folder";
}

/// The committed-file menu (slot `End`). `target.path` is the file, `target.commit` the revision Blame shows it
/// at (the commit that holds the file; for a deleted file its first parent).
pub fn commit_file(target: &MenuTarget, cx: &App) -> Vec<MenuItem> {
    let mut items = Vec::new();
    if let Some(path) = target.path.clone() {
        if let Some(commit) = target.commit {
            let (ctx, file) = (target.tab.clone(), path.clone());
            items.push(
                MenuItem::action(ids::BLAME, "Blame file", move |_, cx| {
                    open_blame(&ctx, file.clone(), commit, cx)
                })
                .with_icon(AppIcon::Blame),
            );
        }
        let (ctx, file) = (target.tab.clone(), path.clone());
        items.push(
            MenuItem::action(ids::HISTORY, "File history", move |_, cx| {
                open_history(&ctx, file.clone(), cx)
            })
            .with_icon(AppIcon::History),
        );
        let (ctx, file) = (target.tab.clone(), path);
        items.push(
            MenuItem::action(
                ids::OPEN_IN_FOLDER,
                "Open file in folder",
                move |window, cx| open_in_folder(&ctx, &file, window, cx),
            )
            .with_icon(AppIcon::FolderOpen),
        );
    }
    extend(cx, MenuSlot::End, target, &mut items);
    items
}

/// Opens the blame view for `path` at `commit` (G8 mounts the view while `ui.blame` is `Some`).
pub fn open_blame(ctx: &TabCtx, path: RelaPath, commit: ObjectId, cx: &mut App) {
    crate::blame::open(ctx, path, BlameRev::Commit(commit), cx);
}

/// Opens the file history view for `path` (it replaces the repository view while `ui.history` is `Some`).
pub fn open_history(ctx: &TabCtx, path: RelaPath, cx: &mut App) {
    crate::file_history::open(ctx, path, cx);
}

fn open_in_folder(ctx: &TabCtx, path: &RelaPath, _window: &mut Window, cx: &mut App) {
    let Some(workdir) = ctx.backend.handle().and_then(|handle| handle.workdir()) else {
        crate::app_view::show_toast(
            cx,
            Some(ctx.id),
            crate::notifications::ToastKind::Warning,
            "This repository has no working directory",
            None,
        );
        return;
    };
    // The existence checks touch the file system (possibly a network drive): run them off the UI thread.
    let path = path.clone();
    let tab = ctx.id;
    let found = crate::backend::bg(cx, move || platform::locate(&workdir, &path));
    cx.spawn(async move |cx| {
        let found = found.await;
        cx.update(|cx| match found {
            Some(target) => platform::open_target(cx, &target),
            None => crate::app_view::show_toast(
                cx,
                Some(tab),
                crate::notifications::ToastKind::Warning,
                "The folder of this file does not exist",
                None,
            ),
        });
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;

    use gpui_kit::TestAppContext;

    use super::*;
    use crate::menus::{MenuKind, register_extender};
    use crate::test_support::{inert_tab_ctx, init_app};

    fn target(cx: &mut gpui_kit::App, with_commit: bool) -> MenuTarget {
        MenuTarget {
            kind: MenuKind::CommitFile,
            tab: inert_tab_ctx(cx),
            branch: None,
            upstream: None,
            commit: with_commit.then(|| {
                ObjectId::from_hex(b"1111111111111111111111111111111111111111").expect("hex")
            }),
            path: Some(RelaPath::from("src/lib.rs")),
            is_current: false,
            checked_out_elsewhere: false,
        }
    }

    fn labels(items: &[MenuItem]) -> Vec<String> {
        items
            .iter()
            .filter_map(|item| match item {
                MenuItem::Action { label, .. } => Some(label.to_string()),
                MenuItem::Separator => None,
            })
            .collect()
    }

    /// 09 §4.8: Blame file, File history, Open file in folder (in this order, all always present).
    #[gpui_kit::test]
    fn three_items_in_order(cx: &mut TestAppContext) {
        init_app(cx);
        cx.update(|cx| {
            let target = target(cx, true);
            let items = commit_file(&target, cx);
            assert_eq!(
                labels(&items),
                vec!["Blame file", "File history", "Open file in folder"]
            );
        });
    }

    /// Without a revision (nothing to blame) the other two items remain.
    #[gpui_kit::test]
    fn without_revision_blame_is_omitted(cx: &mut TestAppContext) {
        init_app(cx);
        cx.update(|cx| {
            let target = target(cx, false);
            assert_eq!(
                labels(&commit_file(&target, cx)),
                vec!["File history", "Open file in folder"]
            );
        });
    }

    /// The `End` slot is open to extenders (M2).
    #[gpui_kit::test]
    fn end_slot_is_extended(cx: &mut TestAppContext) {
        init_app(cx);
        cx.update(|cx| {
            register_extender(cx, MenuKind::CommitFile, MenuSlot::End, |_, items, _| {
                items.push(MenuItem::action("extra", "Extra item", |_, _| {}));
            });
            let target = target(cx, true);
            let shown = labels(&commit_file(&target, cx));
            assert_eq!(shown.last().map(String::as_str), Some("Extra item"));
        });
    }
}
