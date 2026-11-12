//! Icon roles (Gitnuro's icons mapped to Lucide, spec 21 §1.4).
//!
//! Every role resolves to a Lucide icon that `assets.rs` embeds. Fresh artwork is not needed.

use gpui_kit::Hsla;
use gpui_kit::Styled as _;
use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;

use crate::metrics::dp;

macro_rules! app_icons {
    ($($variant:ident => $lucide:ident),* $(,)?) => {
        /// Icon roles used by the application.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum AppIcon {
            $($variant),*
        }

        impl AppIcon {
            /// Every role, in declaration order.
            pub const ALL: &'static [AppIcon] = &[$(AppIcon::$variant),*];

            /// The Lucide icon of this role.
            pub fn name(self) -> IconName {
                match self {
                    $(AppIcon::$variant => IconName::$lucide),*
                }
            }
        }
    };
}

app_icons! {
    Open => FolderOpen,
    Download => Download,
    Upload => Upload,
    Branch => GitBranch,
    Stash => Archive,
    ApplyStash => ArchiveRestore,
    Terminal => SquareTerminal,
    Actions => Zap,
    Settings => Settings,
    ExpandMore => ChevronDown,
    ChevronRight => ChevronRight,
    ArrowUp => ChevronUp,
    Cloud => Cloud,
    Tag => Tag,
    Submodule => Package,
    Search => Search,
    Close => X,
    Add => Plus,
    Remove => Minus,
    Code => Code,
    Bug => Bug,
    Info => Info,
    Warning => TriangleAlert,
    Error => CircleAlert,
    Start => Play,
    Edit => Pencil,
    Delete => Trash,
    Copy => Copy,
    CopyAll => ClipboardCopy,
    Cut => Scissors,
    Paste => ClipboardPaste,
    Message => MessageSquare,
    Refresh => RefreshCw,
    UpdateSubmodule => ArrowDownToLine,
    Sign => Signature,
    ScrollTop => ArrowUpToLine,
    Blame => UserPen,
    History => FileClock,
    Done => ListChecks,
    RemoveDone => ListX,
    Folder => Folder,
    FolderOpen => FolderOpen,
    List => List,
    Tree => ListTree,
    Revert => RotateCcw,
    Undo => Undo2,
    Hunks => SquareSplitVertical,
    FullFile => FileText,
    Unified => Rows2,
    Split => Columns2,
    Binary => Binary,
    Palette => Palette,
    Layout => LayoutDashboard,
    Schedule => Clock,
    Network => Network,
    Password => KeyRound,
    Security => Shield,
    UndoOp => Undo2,
    RedoOp => Redo2,
    Sparkles => Sparkles,
    Spinner => LoaderCircle,
    PullRequest => GitPullRequest,
    PullRequestCreate => GitPullRequestCreate,
    PullRequestDraft => GitPullRequestDraft,
    Merged => GitMerge,
    PullRequestClosed => GitPullRequestClosed,
    ExternalLink => ExternalLink,
    Accounts => Users,
    Github => Github,
    Server => Server,
    Updates => ArrowDownToLine,
    Worktree => FolderTree,
    Lock => Lock,
}

/// An icon element of `size_dp` (square, in dp so it scales with the UI scale) tinted with `color`.
pub fn icon(role: AppIcon, size_dp: f32, color: Hsla) -> Icon {
    Icon::new(role.name()).size(dp(size_dp)).text_color(color)
}
