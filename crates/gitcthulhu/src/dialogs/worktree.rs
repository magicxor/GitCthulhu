//! The Add worktree dialog (D0.7; owned by X6; spec 21 §5.7): the directory (text field with a Browse button), what
//! the worktree checks out (an existing branch, a new branch from a start point, or a detached commit) and an optional
//! lock with a reason. The dialog validates the form off the UI thread (directory free, branch name valid, commit
//! resolvable), shows the first problem in a banner and closes once `worktree add` was handed to the tab.

use std::path::{Path, PathBuf};

use gpui_kit::component::WindowExt as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Subscription, Window, div,
};

use super::IconBasedDialog;
use super::register::common::{self, FocusOnce};
use crate::backend::Repo;
use crate::backend::{
    FullName, GitError, ObjectId, StartPoint, WorktreeAddOptions, WorktreeInfo, WorktreeTarget, bg,
    validate,
};
use crate::icons::AppIcon;
use crate::metrics::dp;
use crate::tab::TabCtx;
use crate::theme::{ActiveTokens as _, TextStyleExt as _};
use crate::widgets::dropdown::{FilterDropdown, FilterDropdownEvent};
use crate::widgets::field::{FieldEvent, OutlinedField};
use crate::widgets::{CheckboxText, IconButton, RadioRow, observed};
use crate::worktrees::metrics::{
    BROWSE_BUTTON, BROWSE_END, CARD_GAP, DIALOG_EST_H, DIALOG_W, LABEL_BOTTOM, LABEL_TOP,
};

/// Element ids of the Add worktree dialog.
pub mod ids {
    /// The dialog content.
    pub const DIALOG: &str = "worktree-dialog";
    /// The directory field.
    pub const PATH: &str = "worktree-path";
    /// The Browse button of the directory field.
    pub const PATH_BROWSE: &str = "worktree-path-browse";
    /// The "Existing branch" card.
    pub const MODE_EXISTING: &str = "worktree-mode-existing";
    /// The "New branch" card.
    pub const MODE_NEW: &str = "worktree-mode-new";
    /// The "Detached commit" card.
    pub const MODE_DETACHED: &str = "worktree-mode-detached";
    /// The dropdown of the existing branches.
    pub const BRANCH: &str = "worktree-branch";
    /// The new branch's name field.
    pub const NEW_NAME: &str = "worktree-new-name";
    /// The dropdown of the start points.
    pub const START: &str = "worktree-start";
    /// The commit field of the detached mode.
    pub const COMMIT: &str = "worktree-commit";
    /// The "Lock" checkbox.
    pub const LOCK: &str = "worktree-lock";
    /// The lock reason field.
    pub const LOCK_REASON: &str = "worktree-lock-reason";
    /// The error banner.
    pub const ERROR: &str = "worktree-error";
}

/// What the new worktree checks out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// An existing local branch that no worktree has checked out.
    Existing,
    /// A new branch created from a start point.
    NewBranch,
    /// A commit (detached HEAD).
    Detached,
}

/// How the dialog starts.
#[derive(Clone, Debug)]
pub enum Preset {
    /// The first free branch (or a new branch when every branch is checked out).
    Default,
    /// This local branch.
    Branch(FullName),
}

/// Opens the Add worktree dialog.
pub fn open_add_worktree(window: &mut Window, cx: &mut App, tab: TabCtx) {
    open_with(window, cx, tab, Preset::Default);
}

/// Opens the dialog for an existing local branch (the branch menu's "Create worktree…").
pub fn open_add_worktree_for_branch(
    window: &mut Window,
    cx: &mut App,
    tab: TabCtx,
    branch: FullName,
) {
    open_with(window, cx, tab, Preset::Branch(branch));
}

/// The validated form as plain data, moved to the background check.
struct Form {
    path: PathBuf,
    mode: Mode,
    branch: Option<FullName>,
    new_name: String,
    start: StartPoint,
    commit: String,
    lock_reason: Option<String>,
}

struct AddWorktreeDialog {
    tab: TabCtx,
    mode: Mode,
    /// The directory new worktrees go to by default and the base of relative paths: next to the repository.
    base_dir: PathBuf,
    repo_name: String,
    path: Entity<OutlinedField>,
    /// The text the dialog itself put in the path field; the field is "touched" once it differs.
    auto_path: String,
    branches: Vec<(SharedString, FullName)>,
    branch_dd: Entity<FilterDropdown>,
    branch_sel: Option<usize>,
    new_name: Entity<OutlinedField>,
    starts: Vec<(SharedString, StartPoint)>,
    start_dd: Entity<FilterDropdown>,
    start_sel: usize,
    commit: Entity<OutlinedField>,
    lock: bool,
    lock_reason: Entity<OutlinedField>,
    error: Option<SharedString>,
    checking: bool,
    /// Bumped on every edit of the form; a background check of an older form is dropped.
    generation: u64,
    focus: FocusOnce,
    _subscriptions: Vec<Subscription>,
}

/// File-name-safe form of a branch name.
fn slug(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | ' ' => '-',
            c => c,
        })
        .collect::<String>()
        .trim_matches(['-', '.'])
        .to_owned()
}

/// Where a worktree for `slug` goes by default: `<base>/<repo>-<slug>`.
fn default_path(base: &Path, repo: &str, slug: &str) -> PathBuf {
    let name = if slug.is_empty() {
        format!("{repo}-worktree")
    } else {
        format!("{repo}-{slug}")
    };
    base.join(name)
}

impl AddWorktreeDialog {
    fn text(field: &Entity<OutlinedField>, cx: &App) -> String {
        common::text_of(field, cx)
    }

    fn suggestion(&self, cx: &App) -> String {
        let slug = match self.mode {
            Mode::Existing => self
                .branch_sel
                .and_then(|ix| self.branches.get(ix))
                .map(|(name, _)| slug(name))
                .unwrap_or_default(),
            Mode::NewBranch => slug(Self::text(&self.new_name, cx).trim()),
            Mode::Detached => "detached".to_owned(),
        };
        default_path(&self.base_dir, &self.repo_name, &slug)
            .to_string_lossy()
            .into_owned()
    }

    /// Puts the suggested directory into the path field unless the user typed their own.
    fn follow_suggestion(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = Self::text(&self.path, cx);
        if !self.auto_path.is_empty() && current != self.auto_path {
            return;
        }
        let suggestion = self.suggestion(cx);
        self.auto_path = suggestion.clone();
        self.path
            .update(cx, |field, cx| field.set_text(&suggestion, window, cx));
    }

    /// The form changed: the shown problem is stale and so is a running check.
    fn touched(&mut self) {
        self.error = None;
        self.generation += 1;
    }

    fn set_mode(&mut self, mode: Mode, window: &mut Window, cx: &mut Context<Self>) {
        if self.mode == mode {
            return;
        }
        self.mode = mode;
        self.touched();
        self.follow_suggestion(window, cx);
        cx.notify();
    }

    fn valid(&self, cx: &App) -> bool {
        if self.checking || Self::text(&self.path, cx).trim().is_empty() {
            return false;
        }
        match self.mode {
            Mode::Existing => self.branch_sel.is_some(),
            Mode::NewBranch => !Self::text(&self.new_name, cx).trim().is_empty(),
            Mode::Detached => !Self::text(&self.commit, cx).trim().is_empty(),
        }
    }

    fn form(&self, cx: &App) -> Form {
        let raw = PathBuf::from(Self::text(&self.path, cx).trim());
        let path = if raw.is_absolute() {
            raw
        } else {
            self.base_dir.join(raw)
        };
        let reason = Self::text(&self.lock_reason, cx).trim().to_owned();
        Form {
            path,
            mode: self.mode,
            branch: self
                .branch_sel
                .and_then(|ix| self.branches.get(ix))
                .map(|(_, full)| full.clone()),
            new_name: Self::text(&self.new_name, cx).trim().to_owned(),
            start: self
                .starts
                .get(self.start_sel)
                .map_or(StartPoint::Head, |(_, s)| s.clone()),
            commit: Self::text(&self.commit, cx).trim().to_owned(),
            lock_reason: self.lock.then_some(reason),
        }
    }

    /// Validates the form in the background, then hands the operation to the tab (or shows the problem).
    fn accept(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.valid(cx) {
            return;
        }
        let form = self.form(cx);
        let repo = self.tab.backend.repo().cloned();
        self.checking = true;
        self.error = None;
        let generation = self.generation;
        cx.notify();
        let task = bg(cx, move || check(repo.as_ref(), form));
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |dialog, window, cx| {
                dialog.checked(generation, result, window, cx)
            })
            .ok();
        })
        .detach();
    }

    fn checked(
        &mut self,
        generation: u64,
        result: Result<WorktreeAddOptions, SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.checking = false;
        if generation != self.generation {
            // The form was edited while the check ran: its result describes a form that no longer exists.
            cx.notify();
            return;
        }
        match result {
            Ok(opts) => {
                window.close_dialog(cx);
                self.tab.with_tab(window, cx, move |tab, window, cx| {
                    tab.add_worktree(opts, window, cx);
                });
            }
            Err(message) => {
                self.error = Some(message);
                cx.notify();
            }
        }
    }

    fn browse(&self, window: &mut Window, cx: &mut App) {
        let field = self.path.clone();
        common::pick_folder(window, cx, move |path, window, cx| {
            let text = path.to_string_lossy().into_owned();
            field.update(cx, |f, cx| f.set_text(&text, window, cx));
        });
    }
}

/// The background half of "Add worktree": every check that touches the file system or the repository.
fn check(repo: Option<&Repo>, form: Form) -> Result<WorktreeAddOptions, SharedString> {
    match std::fs::metadata(&form.path) {
        Ok(meta) if !meta.is_dir() => {
            return Err(format!(
                "'{}' already exists and is not a directory",
                form.path.display()
            )
            .into());
        }
        Ok(_) => {
            let occupied = std::fs::read_dir(&form.path)
                .map(|mut entries| entries.next().is_some())
                .unwrap_or(false);
            if occupied {
                return Err(format!("The directory '{}' is not empty", form.path.display()).into());
            }
        }
        Err(_) => {}
    }
    let target = match form.mode {
        Mode::Existing => {
            let branch = form
                .branch
                .ok_or_else(|| SharedString::from("Choose a branch"))?;
            WorktreeTarget::ExistingBranch(branch)
        }
        Mode::NewBranch => {
            if let Some(repo) = repo {
                validate::validate_branch_name(repo, &form.new_name).map_err(explain)?;
            }
            WorktreeTarget::NewBranch {
                name: form.new_name,
                start: form.start,
            }
        }
        Mode::Detached => {
            let repo = repo.ok_or_else(|| SharedString::from("The repository is not available"))?;
            let id: ObjectId = validate::resolve_commit(repo, &form.commit).map_err(explain)?;
            WorktreeTarget::Detached(id)
        }
    };
    Ok(WorktreeAddOptions {
        path: form.path,
        target,
        lock_reason: form.lock_reason,
    })
}

fn explain(err: GitError) -> SharedString {
    err.to_string().into()
}

impl Render for AddWorktreeDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tokens = *cx.tokens();
        self.focus.run(&self.path, false, window, cx);
        let this = cx.weak_entity();
        let label = |text: &'static str| {
            div()
                .pt(dp(LABEL_TOP))
                .pb(dp(LABEL_BOTTOM))
                .child(common::form_label(text, &tokens))
        };
        let card_w = (DIALOG_W - 2. * CARD_GAP) / 3.;
        let card = |id: &'static str, title: &'static str, mode: Mode| {
            let this = this.clone();
            RadioRow::new(id, title, self.mode == mode)
                .width(card_w)
                .on_select(move |window, cx| {
                    this.update(cx, |d, cx| d.set_mode(mode, window, cx)).ok();
                })
        };
        let browse = {
            let this = this.clone();
            IconButton::new(ids::PATH_BROWSE, AppIcon::FolderOpen)
                .tooltip("Browse")
                .on_click(move |window, cx| {
                    this.update(cx, |d, cx| d.browse(window, cx)).ok();
                })
        };
        let lock_toggle = this.clone();
        let content = observed(ids::DIALOG)
            .w(dp(DIALOG_W))
            .flex()
            .flex_col()
            .child(label("Directory"))
            .child(
                div().relative().w_full().child(self.path.clone()).child(
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .right(dp(BROWSE_END))
                        .flex()
                        .items_center()
                        .child(browse),
                ),
            )
            .child(label("Check out"))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(dp(CARD_GAP))
                    .child(card(ids::MODE_EXISTING, "Existing branch", Mode::Existing))
                    .child(card(ids::MODE_NEW, "New branch", Mode::NewBranch))
                    .child(card(ids::MODE_DETACHED, "Detached commit", Mode::Detached)),
            )
            .when(self.mode == Mode::Existing, |col| {
                col.child(label("Branch"))
                    .child(self.branch_dd.clone())
                    .when(self.branches.is_empty(), |col| {
                        col.child(
                            div()
                                .pt(dp(8.))
                                .text_body2()
                                .text_color(tokens.on_background_secondary)
                                .child("Every local branch is checked out in a worktree."),
                        )
                    })
            })
            .when(self.mode == Mode::NewBranch, |col| {
                col.child(label("New branch name"))
                    .child(self.new_name.clone())
                    .child(label("Start point"))
                    .child(self.start_dd.clone())
            })
            .when(self.mode == Mode::Detached, |col| {
                col.child(label("Commit")).child(self.commit.clone())
            })
            .child(div().pt(dp(LABEL_TOP)).child(
                CheckboxText::new(ids::LOCK, self.lock, "Lock the worktree").on_toggle(
                    move |checked, _, cx| {
                        lock_toggle
                            .update(cx, |d, cx| {
                                d.lock = checked;
                                d.touched();
                                cx.notify();
                            })
                            .ok();
                    },
                ),
            ))
            .when(self.lock, |col| {
                col.child(div().pt(dp(LABEL_BOTTOM)).child(self.lock_reason.clone()))
            })
            .when_some(self.error.clone(), |col, error| {
                col.child(observed(ids::ERROR).child(common::error_banner(error, &tokens)))
            });
        let accept = this;
        IconBasedDialog::new(AppIcon::Worktree, "Add worktree", "Add worktree")
            .subtitle("Check out another branch or commit into its own directory")
            .primary_enabled(self.valid(cx))
            .content(content)
            .on_primary(move |window, cx| {
                accept.update(cx, |d, cx| d.accept(window, cx)).ok();
            })
    }
}

/// What the dialog reads from the tab's model.
struct Choices {
    base_dir: PathBuf,
    repo_name: String,
    branches: Vec<(SharedString, FullName)>,
    starts: Vec<(SharedString, StartPoint)>,
    /// The commit selected in the log, offered in the detached mode.
    selected_commit: Option<ObjectId>,
}

/// Reads the branches that are free to check out, the start points and the repository's place from the model.
fn choices(tab: &TabCtx, cx: &App) -> Choices {
    let workdir = tab.backend.handle().and_then(|h| h.workdir());
    let model = tab.model.read(cx);
    let list: &[WorktreeInfo] = model
        .worktrees
        .value
        .as_ref()
        .map_or(&[][..], |list| list.as_slice());
    let main = list.iter().find(|w| w.id.is_none()).map(|w| w.path.clone());
    let root = workdir.or(main).unwrap_or_else(|| PathBuf::from("."));
    let base_dir = root
        .parent()
        .map_or_else(|| root.clone(), Path::to_path_buf);
    let repo_name = root.file_name().map_or_else(
        || "repository".to_owned(),
        |n| n.to_string_lossy().into_owned(),
    );
    // The main worktree's name stays the repository's name when the dialog opens in a linked worktree's tab.
    let repo_name = main_name(list).unwrap_or(repo_name);

    let occupied: Vec<FullName> = list
        .iter()
        .filter_map(crate::worktrees::checked_out_branch)
        .collect();
    let mut branches = Vec::new();
    let mut starts = vec![("HEAD (current commit)".into(), StartPoint::Head)];
    if let Some(refs) = model.refs.value.as_ref() {
        for branch in &refs.local {
            let taken = branch.is_head
                || branch.checked_out_in.is_some()
                || occupied.contains(&branch.full);
            if !taken {
                branches.push((SharedString::from(branch.name.clone()), branch.full.clone()));
            }
            starts.push((
                SharedString::from(branch.name.clone()),
                StartPoint::Ref(branch.full.clone()),
            ));
        }
        for remote in &refs.remotes {
            for branch in &remote.branches {
                starts.push((
                    SharedString::from(format!("{}/{}", remote.name, branch.name)),
                    StartPoint::Ref(branch.full.clone()),
                ));
            }
        }
        for tag in &refs.tags {
            starts.push((
                SharedString::from(format!("tag {}", tag.name)),
                StartPoint::Ref(tag.full.clone()),
            ));
        }
    }
    let selected_commit = tab.ui.read(cx).selection.commit().map(|(id, _)| id);
    Choices {
        base_dir,
        repo_name,
        branches,
        starts,
        selected_commit,
    }
}

/// The directory name of the main worktree.
fn main_name(list: &[WorktreeInfo]) -> Option<String> {
    let main = list.iter().find(|w| w.id.is_none() && !w.bare)?;
    main.path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
}

/// Opens the dialog with `preset`.
pub(crate) fn open_with(window: &mut Window, cx: &mut App, tab: TabCtx, preset: Preset) {
    let Choices {
        base_dir,
        repo_name,
        branches,
        starts,
        selected_commit,
    } = choices(&tab, cx);
    let branch_sel = match &preset {
        Preset::Branch(full) => branches.iter().position(|(_, f)| f == full),
        Preset::Default => (!branches.is_empty()).then_some(0),
    }
    .or_else(|| (!branches.is_empty()).then_some(0));
    let mode = if branch_sel.is_some() {
        Mode::Existing
    } else {
        Mode::NewBranch
    };

    // The Browse button is overlaid on the field's end: the field keeps the text clear of it.
    let path = cx.new(|cx| {
        OutlinedField::new(ids::PATH, window, cx)
            .trailing_space(BROWSE_BUTTON)
            .hint("Directory of the new worktree", window, cx)
    });
    let new_name = common::new_field(ids::NEW_NAME, Some("feature/name"), "", window, cx);
    let commit_text = selected_commit.map(|id| id.to_string()).unwrap_or_default();
    let commit = common::new_field(
        ids::COMMIT,
        Some("Commit id, branch, tag or revision such as HEAD~2"),
        &commit_text,
        window,
        cx,
    );
    let lock_reason =
        common::new_field(ids::LOCK_REASON, Some("Reason (optional)"), "", window, cx);
    let branch_names = branches.iter().map(|(n, _)| n.clone()).collect();
    let branch_dd = cx.new(|cx| {
        let mut dd = FilterDropdown::new(ids::BRANCH, branch_names, branch_sel, window, cx);
        dd.set_width(DIALOG_W, cx);
        dd
    });
    let start_names = starts.iter().map(|(n, _)| n.clone()).collect();
    let start_dd = cx.new(|cx| {
        let mut dd = FilterDropdown::new(ids::START, start_names, Some(0), window, cx);
        dd.set_width(DIALOG_W, cx);
        dd
    });

    let view = cx.new(|cx| {
        let mut subscriptions = Vec::new();
        subscriptions.push(cx.subscribe_in(
            &branch_dd,
            window,
            |this: &mut AddWorktreeDialog, _, event: &FilterDropdownEvent, window, cx| {
                let FilterDropdownEvent::Selected(ix) = event;
                this.branch_sel = Some(*ix);
                this.touched();
                this.follow_suggestion(window, cx);
                cx.notify();
            },
        ));
        subscriptions.push(cx.subscribe(
            &start_dd,
            |this: &mut AddWorktreeDialog, _, event: &FilterDropdownEvent, cx| {
                let FilterDropdownEvent::Selected(ix) = event;
                this.start_sel = *ix;
                this.touched();
                cx.notify();
            },
        ));
        subscriptions.push(cx.subscribe_in(
            &new_name,
            window,
            |this: &mut AddWorktreeDialog, _, event: &FieldEvent, window, cx| match event {
                FieldEvent::Changed => {
                    this.touched();
                    this.follow_suggestion(window, cx);
                    cx.notify();
                }
                FieldEvent::Enter => this.accept(window, cx),
            },
        ));
        for field in [&path, &commit, &lock_reason] {
            subscriptions.push(cx.subscribe_in(
                field,
                window,
                |this: &mut AddWorktreeDialog, _, event: &FieldEvent, window, cx| match event {
                    FieldEvent::Changed => {
                        this.touched();
                        cx.notify();
                    }
                    FieldEvent::Enter => this.accept(window, cx),
                },
            ));
        }
        let mut dialog = AddWorktreeDialog {
            tab,
            mode,
            base_dir,
            repo_name,
            path,
            auto_path: String::new(),
            branches,
            branch_dd,
            branch_sel,
            new_name,
            starts,
            start_dd,
            start_sel: 0,
            commit,
            lock: false,
            lock_reason,
            error: None,
            checking: false,
            generation: 0,
            focus: FocusOnce::default(),
            _subscriptions: subscriptions,
        };
        dialog.follow_suggestion(window, cx);
        dialog
    });
    common::open_view(window, cx, DIALOG_W, DIALOG_EST_H, view);
}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;

    use super::*;

    #[test]
    fn slug_makes_branch_names_file_names() {
        assert_eq!(slug("feature/login"), "feature-login");
        assert_eq!(slug("fix: a?b"), "fix--a-b");
        assert_eq!(slug("-.x.-"), "x");
        assert_eq!(slug(""), "");
    }

    #[test]
    fn default_path_sits_next_to_the_repository() {
        let base = Path::new("base");
        assert_eq!(
            default_path(base, "repo", "topic-x"),
            base.join("repo-topic-x")
        );
        assert_eq!(default_path(base, "repo", ""), base.join("repo-worktree"));
    }

    #[test]
    fn check_rejects_a_directory_that_is_not_empty() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("f"), "x").expect("write");
        let form = |path: PathBuf| Form {
            path,
            mode: Mode::NewBranch,
            branch: None,
            new_name: "topic".into(),
            start: StartPoint::Head,
            commit: String::new(),
            lock_reason: None,
        };
        let err = check(None, form(dir.path().to_owned())).expect_err("not empty");
        assert!(err.contains("not empty"), "{err}");
        let file = dir.path().join("f");
        let err = check(None, form(file)).expect_err("a file");
        assert!(err.contains("not a directory"), "{err}");
        let ok = check(None, form(dir.path().join("fresh"))).expect("a free path");
        assert!(matches!(ok.target, WorktreeTarget::NewBranch { .. }));
        let empty = dir.path().join("empty");
        std::fs::create_dir(&empty).expect("mkdir");
        check(None, form(empty)).expect("an empty directory is fine");
    }
}
