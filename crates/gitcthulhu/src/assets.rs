//! The composite asset source: embedded fonts and app SVGs, the Lucide icons of [`crate::icons::AppIcon`], and the
//! kit's default icons last (the kit source errors on unknown paths, so it must come last; spec 21 §1.4).

use std::borrow::Cow;

use gpui_kit::{AssetSource, Result, SharedString};

#[derive(rust_embed::RustEmbed)]
#[folder = "$CARGO_MANIFEST_DIR/assets"]
#[include = "icons/**/*.svg"]
#[include = "fonts/**/*"]
struct AppFiles;

// One entry per distinct Lucide file used by `AppIcon::name`. The `app_icons_are_embedded` test keeps the list
// complete.
gpui_kit::assets::icon_assets!(
    GitIcons,
    [
        FolderOpen,
        Download,
        Upload,
        GitBranch,
        Archive,
        ArchiveRestore,
        SquareTerminal,
        Zap,
        Settings,
        ChevronDown,
        ChevronRight,
        ChevronUp,
        Cloud,
        Tag,
        Package,
        Search,
        X,
        Plus,
        Minus,
        Code,
        Bug,
        Info,
        TriangleAlert,
        CircleAlert,
        Play,
        Pencil,
        Trash,
        Copy,
        ClipboardCopy,
        Scissors,
        ClipboardPaste,
        MessageSquare,
        RefreshCw,
        ArrowDownToLine,
        Signature,
        ArrowUpToLine,
        UserPen,
        FileClock,
        ListChecks,
        ListX,
        Folder,
        List,
        ListTree,
        RotateCcw,
        Undo2,
        SquareSplitVertical,
        FileText,
        Rows2,
        Columns2,
        Binary,
        Palette,
        LayoutDashboard,
        Clock,
        Network,
        KeyRound,
        Shield,
        Redo2,
        Sparkles,
        LoaderCircle,
        GitPullRequest,
        GitPullRequestCreate,
        GitPullRequestDraft,
        GitMerge,
        GitPullRequestClosed,
        ExternalLink,
        Users,
        Github,
        Server,
        FolderTree,
        Lock,
    ]
);

/// Asset source of the application window (`gpui_kit::application().with_assets(AppAssets)`).
#[derive(Clone, Copy, Debug, Default)]
pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(file) = AppFiles::get(path) {
            return Ok(Some(file.data));
        }
        if let Some(bytes) = GitIcons.load(path)? {
            return Ok(Some(bytes));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut all: Vec<SharedString> = AppFiles::iter()
            .filter(|p| p.starts_with(path))
            .map(|p| SharedString::from(p.to_string()))
            .collect();
        all.extend(GitIcons.list(path)?);
        all.extend(gpui_kit::assets::Assets.list(path)?);
        all.sort();
        all.dedup();
        Ok(all)
    }
}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;

    use super::*;
    use crate::icons::AppIcon;

    #[test]
    fn app_icons_are_embedded() {
        for role in AppIcon::ALL {
            let path = role.name().path();
            let loaded = GitIcons.load(path.as_ref()).expect("icon source");
            assert!(
                loaded.is_some_and(|b| !b.is_empty()),
                "{role:?} ({path}) must be embedded by GitIcons"
            );
        }
    }

    #[test]
    fn unknown_paths_fall_through_to_the_kit_source() {
        let assets = AppAssets;
        assert!(
            assets.load("icons/definitely-not-an-icon.svg").is_err()
                || assets
                    .load("icons/definitely-not-an-icon.svg")
                    .is_ok_and(|v| v.is_none()),
            "unknown paths are not found"
        );
        assert!(
            assets
                .load("fonts/Inter-Regular.ttf")
                .is_ok_and(|v| v.is_some()),
            "embedded fonts are served"
        );
    }
}
