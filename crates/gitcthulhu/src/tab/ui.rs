//! `RepoUi`: the view state of a tab — selection, file selection, closeable-view stack, toggles — and the rules
//! that decide what the centre and right panes show (spec 21 §2.3, Gitnuro 08 §1, §4.5).

use std::collections::HashSet;
use std::sync::Arc;

use gpui_kit::{Context, EventEmitter, Modifiers, SharedString};
use indexmap::IndexSet;

use crate::backend::{
    Against, BlameRev, CommitFile, DiffTarget, FullName, ObjectId, RebasePlan, RelaPath, Status,
};
use crate::settings::Settings;
use crate::tab::model::RepoModel;

/// What the log/side panel has selected.
#[derive(Clone, Debug, PartialEq)]
pub enum Selection {
    None,
    /// The initial selection on open (08 §1.2).
    UncommittedChanges,
    Commit {
        id: ObjectId,
        is_stash: bool,
        /// Scroll the log to the commit (only if it is not visible, 08 §9.5 fix).
        scroll_to: bool,
    },
    /// Side panel / chip selection; shows the target commit.
    Branch {
        full: FullName,
        target: ObjectId,
    },
    Tag {
        full: FullName,
        target: ObjectId,
    },
    Stash {
        id: ObjectId,
        index: usize,
    },
}

impl Selection {
    /// The commit a commit-based selection shows, with its stash flag.
    pub fn commit(&self) -> Option<(ObjectId, bool)> {
        match self {
            Selection::Commit { id, is_stash, .. } => Some((*id, *is_stash)),
            Selection::Branch { target, .. } | Selection::Tag { target, .. } => {
                Some((*target, false))
            }
            Selection::Stash { id, .. } => Some((*id, true)),
            Selection::None | Selection::UncommittedChanges => None,
        }
    }
}

/// The file lists a file selection can live in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FileList {
    Staged,
    Unstaged,
    CommitFiles,
}

/// The selected files: always in exactly one list (08 §4.5).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FileSelection {
    pub list: Option<FileList>,
    /// Insertion order = "most recently added" for Shift-clicks.
    pub paths: IndexSet<RelaPath>,
    pub anchor: Option<RelaPath>,
}

impl FileSelection {
    /// The single selected file of `list`, if exactly one is selected there.
    pub fn single(&self, list: FileList) -> Option<&RelaPath> {
        (self.list == Some(list) && self.paths.len() == 1)
            .then(|| self.paths.first())
            .flatten()
    }

    /// Whether `path` is selected in `list`.
    pub fn contains(&self, list: FileList, path: &RelaPath) -> bool {
        self.list == Some(list) && self.paths.contains(path)
    }
}

/// Views that Escape closes, most recently opened first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseableView {
    SidePaneSearch,
    LogSearch,
    StagedSearch,
    UnstagedSearch,
    CommitChangesSearch,
    Diff,
    Blame,
    FileHistory,
}

/// LIFO of open closeable views; `push` moves an existing entry to the top.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ViewStack(Vec<CloseableView>);

impl ViewStack {
    /// Puts `v` on top (moving it if it is already open).
    pub fn push(&mut self, v: CloseableView) {
        self.remove(v);
        self.0.push(v);
    }

    /// Removes `v` wherever it is.
    pub fn remove(&mut self, v: CloseableView) {
        self.0.retain(|x| *x != v);
    }

    /// Takes the most recently opened view.
    pub fn pop(&mut self) -> Option<CloseableView> {
        self.0.pop()
    }

    /// Whether `v` is open.
    pub fn contains(&self, v: CloseableView) -> bool {
        self.0.contains(&v)
    }

    /// The open views, oldest first.
    pub fn as_slice(&self) -> &[CloseableView] {
        &self.0
    }
}

/// The file shown by the blame view.
#[derive(Clone, Debug, PartialEq)]
pub struct BlameTarget {
    pub path: RelaPath,
    pub rev: BlameRev,
    pub minimized: bool,
}

/// State of the log search bar.
#[derive(Clone, Debug, Default)]
pub struct LogSearchUi {
    pub query: SharedString,
    /// Layout row indices of the matches.
    pub matches: Arc<Vec<usize>>,
    pub index: usize,
}

/// Which side-panel sections are expanded.
#[derive(Clone, Debug, PartialEq)]
pub struct SideExpansion {
    pub local: bool,
    pub remotes: bool,
    pub tags: bool,
    pub stashes: bool,
    pub submodules: bool,
    pub worktrees: bool,
}

impl Default for SideExpansion {
    /// 07 §6.3: local branches, stashes and submodules expanded; remotes, tags and worktrees collapsed.
    fn default() -> Self {
        Self {
            local: true,
            remotes: false,
            tags: false,
            stashes: true,
            submodules: true,
            worktrees: false,
        }
    }
}

/// The view state of a tab.
pub struct RepoUi {
    pub selection: Selection,
    pub files: FileSelection,
    pub views: ViewStack,
    /// The commit box's Amend checkbox.
    pub amend: bool,
    pub blame: Option<BlameTarget>,
    /// File history (replaces everything under the toolbar).
    pub history: Option<RelaPath>,
    /// The interactive rebase being edited (08 §6, 20 §5.6); never persisted.
    pub rebase_plan: Option<RebasePlan>,
    pub log_search: Option<LogSearchUi>,
    pub side_filter: SharedString,
    pub side_expanded: SideExpansion,
    pub remotes_collapsed: HashSet<String>,
    pub staged_filter: Option<SharedString>,
    pub unstaged_filter: Option<SharedString>,
    pub commit_files_filter: Option<SharedString>,
    /// The changed files of the commit the commit changes view shows, with that commit (08 §5.2). Written by the
    /// commit changes view when its details load; [`Self::diff_target`] takes renames (and the untracked files of
    /// a stash) from it.
    pub commit_files: Option<(ObjectId, Arc<Vec<CommitFile>>)>,
    /// Extra graph column width in dp; per tab, not persisted (08 §2.2).
    pub graph_padding: f32,
    /// D0.4 slot (always false in milestone 1).
    pub terminal_visible: bool,
}

/// What changed in `RepoUi`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepoUiEvent {
    SelectionChanged,
    FilesChanged,
    CenterChanged,
    ViewStackChanged,
    TogglesChanged,
}

impl EventEmitter<RepoUiEvent> for RepoUi {}

/// What the centre pane shows (08 §1.1).
#[derive(Clone, Debug, PartialEq)]
pub enum CenterContent {
    /// `rebase_plan.is_some()` and no single file selected.
    RebaseInteractive,
    /// `blame.is_some()` and not minimized.
    Blame,
    LogAndDiff {
        log: bool,
        diff: Option<DiffTarget>,
        minimized_blame: bool,
    },
}

/// What the right pane shows (08 §1.2).
#[derive(Clone, Debug, PartialEq)]
pub enum RightContent {
    CommitChanges { commit: ObjectId, stash: bool },
    Status,
    Empty,
}

impl Default for RepoUi {
    fn default() -> Self {
        Self::new()
    }
}

impl RepoUi {
    /// The state of a freshly opened tab: uncommitted changes selected (08 §1.2).
    pub fn new() -> Self {
        Self {
            selection: Selection::UncommittedChanges,
            files: FileSelection::default(),
            views: ViewStack::default(),
            amend: false,
            blame: None,
            history: None,
            rebase_plan: None,
            log_search: None,
            side_filter: SharedString::default(),
            side_expanded: SideExpansion::default(),
            remotes_collapsed: HashSet::new(),
            staged_filter: None,
            unstaged_filter: None,
            commit_files_filter: None,
            commit_files: None,
            graph_padding: 0.,
            terminal_visible: false,
        }
    }

    /// 08 §1.1: the diff is shown iff exactly one file is selected; the log iff `show_diff_with_log` or no diff.
    pub fn center(&self, settings: &Settings, model: &RepoModel) -> CenterContent {
        let diff = self.diff_target(model);
        if self.rebase_plan.is_some() && diff.is_none() {
            return CenterContent::RebaseInteractive;
        }
        if self.blame.as_ref().is_some_and(|b| !b.minimized) {
            return CenterContent::Blame;
        }
        CenterContent::LogAndDiff {
            log: settings.show_diff_with_log || diff.is_none(),
            minimized_blame: self.blame.as_ref().is_some_and(|b| b.minimized),
            diff,
        }
    }

    /// 08 §1.2: commit-based selections show CommitChanges (Branch/Tag use their target commit); otherwise the
    /// status pane, except while an interactive rebase is being edited.
    pub fn right(&self) -> RightContent {
        if let Some((commit, stash)) = self.selection.commit() {
            return RightContent::CommitChanges { commit, stash };
        }
        if self.rebase_plan.is_none() {
            RightContent::Status
        } else {
            RightContent::Empty
        }
    }

    /// The diff target of the single selected file: Staged/Unstaged from the status entries (renames keep their
    /// previous path), commit files against the first parent.
    pub fn diff_target(&self, model: &RepoModel) -> Option<DiffTarget> {
        if self.files.paths.len() != 1 {
            return None;
        }
        let path = self.files.paths.first()?.clone();
        match self.files.list? {
            FileList::Staged | FileList::Unstaged => {
                let status = model.status.value.as_ref()?;
                let staged = self.files.list == Some(FileList::Staged);
                let entries = if staged {
                    &status.staged
                } else {
                    &status.unstaged
                };
                let entry = entries.iter().find(|e| e.path == path)?;
                let previous_path = entry.previous_path.clone();
                Some(if staged {
                    DiffTarget::Staged {
                        path,
                        previous_path,
                    }
                } else {
                    DiffTarget::Unstaged {
                        path,
                        previous_path,
                    }
                })
            }
            FileList::CommitFiles => {
                let (commit, _) = self.selection.commit()?;
                if let Some((owner, files)) = &self.commit_files
                    && *owner == commit
                    && let Some(file) = files.iter().find(|f| f.path == path)
                {
                    return Some(file.target.clone());
                }
                Some(DiffTarget::Commit {
                    commit,
                    against: Against::FirstParent,
                    path,
                    previous_path: None,
                })
            }
        }
    }

    /// Selects `sel`. The file selection is cleared when the right pane's content changes.
    pub fn select(&mut self, sel: Selection, cx: &mut Context<Self>) {
        if self.selection == sel {
            return;
        }
        let before = self.right();
        self.selection = sel;
        if self.right() != before && self.files.list.is_some() {
            self.files = FileSelection::default();
            self.views.remove(CloseableView::Diff);
            cx.emit(RepoUiEvent::FilesChanged);
        }
        cx.emit(RepoUiEvent::SelectionChanged);
        cx.emit(RepoUiEvent::CenterChanged);
        cx.notify();
    }

    /// 08 §4.5 click rules. Plain click selects only `path` (and clears any other list); Ctrl (`secondary`) toggles;
    /// Shift adds the range between the most recently added selected path and `path` in `visible_order` (the
    /// list's file paths in display order, directories excluded by the caller). `CommitFiles` has single selection
    /// only (08 §5.2).
    pub fn click_file(
        &mut self,
        list: FileList,
        path: RelaPath,
        mods: Modifiers,
        visible_order: &[RelaPath],
        cx: &mut Context<Self>,
    ) {
        let same_list = self.files.list == Some(list) && !self.files.paths.is_empty();
        let multi = list != FileList::CommitFiles && same_list;
        if multi && mods.secondary() {
            if !self.files.paths.shift_remove(&path) {
                self.files.paths.insert(path.clone());
                self.files.anchor = Some(path);
            } else if self.files.anchor.as_ref() == Some(&path) {
                self.files.anchor = self.files.paths.last().cloned();
            }
        } else if multi && mods.shift {
            let last = self.files.paths.last().cloned();
            let from = last.and_then(|l| visible_order.iter().position(|p| *p == l));
            let to = visible_order.iter().position(|p| *p == path);
            match (from, to) {
                (Some(from), Some(to)) => {
                    let range: Vec<RelaPath> = if from <= to {
                        visible_order[from..=to].to_vec()
                    } else {
                        visible_order[to..=from].iter().rev().cloned().collect()
                    };
                    for p in range {
                        self.files.paths.shift_remove(&p);
                        self.files.paths.insert(p);
                    }
                }
                _ => {
                    self.files.paths.insert(path.clone());
                }
            }
            self.files.anchor = Some(path);
        } else {
            self.files = FileSelection {
                list: Some(list),
                paths: IndexSet::from([path.clone()]),
                anchor: Some(path),
            };
        }
        if self.files.paths.is_empty() {
            self.files = FileSelection::default();
        }
        self.sync_diff_view();
        cx.emit(RepoUiEvent::FilesChanged);
        cx.emit(RepoUiEvent::CenterChanged);
        cx.notify();
    }

    /// Clears the file selection (closes the diff).
    pub fn clear_files(&mut self, cx: &mut Context<Self>) {
        if self.files == FileSelection::default() {
            return;
        }
        self.files = FileSelection::default();
        self.sync_diff_view();
        cx.emit(RepoUiEvent::FilesChanged);
        cx.emit(RepoUiEvent::CenterChanged);
        cx.notify();
    }

    /// Drops selected paths that vanished from the status lists (08 §4.5).
    pub fn prune_files(&mut self, model: &RepoModel, cx: &mut Context<Self>) {
        let status = model.status.value.clone();
        self.prune_paths(status.as_deref(), cx);
    }

    /// [`Self::prune_files`] against a status (for callers that cannot borrow the model during the update).
    pub fn prune_paths(&mut self, status: Option<&Status>, cx: &mut Context<Self>) {
        let Some(list @ (FileList::Staged | FileList::Unstaged)) = self.files.list else {
            return;
        };
        let Some(status) = status else {
            return;
        };
        let entries = if list == FileList::Staged {
            &status.staged
        } else {
            &status.unstaged
        };
        let before = self.files.paths.len();
        self.files
            .paths
            .retain(|p| entries.iter().any(|e| e.path == *p));
        if self.files.paths.len() == before {
            return;
        }
        if self
            .files
            .anchor
            .as_ref()
            .is_some_and(|a| !self.files.paths.contains(a))
        {
            self.files.anchor = self.files.paths.last().cloned();
        }
        if self.files.paths.is_empty() {
            self.files = FileSelection::default();
        }
        self.sync_diff_view();
        cx.emit(RepoUiEvent::FilesChanged);
        cx.emit(RepoUiEvent::CenterChanged);
        cx.notify();
    }

    /// Records that `v` was opened (Escape closes the most recent one first).
    pub fn push_view(&mut self, v: CloseableView, cx: &mut Context<Self>) {
        self.views.push(v);
        cx.emit(RepoUiEvent::ViewStackChanged);
        cx.notify();
    }

    /// Escape: closes the most recently opened closeable view and applies its effect (search cleared, diff,
    /// blame or history closed). `false` when nothing is open.
    pub fn close_last_view(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(view) = self.views.pop() else {
            return false;
        };
        match view {
            CloseableView::SidePaneSearch => self.side_filter = SharedString::default(),
            CloseableView::LogSearch => self.log_search = None,
            CloseableView::StagedSearch => self.staged_filter = None,
            CloseableView::UnstagedSearch => self.unstaged_filter = None,
            CloseableView::CommitChangesSearch => self.commit_files_filter = None,
            CloseableView::Diff => {
                self.files = FileSelection::default();
                cx.emit(RepoUiEvent::FilesChanged);
            }
            CloseableView::Blame => self.blame = None,
            CloseableView::FileHistory => self.history = None,
        }
        cx.emit(RepoUiEvent::ViewStackChanged);
        cx.emit(RepoUiEvent::CenterChanged);
        cx.notify();
        true
    }

    /// Keeps the `Diff` entry of the view stack in step with "exactly one file selected".
    fn sync_diff_view(&mut self) {
        if self.files.paths.len() == 1 {
            self.views.push(CloseableView::Diff);
        } else {
            self.views.remove(CloseableView::Diff);
        }
    }
}
