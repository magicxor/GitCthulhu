//! Configuration operations of the catalogue (spec 21 §2.11; owned by G10): saving the author identity and writing
//! the commit-graph file.

use gpui_kit::{App, Context, Window};

use super::{Cancellable, OpKind, OpSpec};
use crate::backend::{ConfigScope, Identity, OpOutcome, RefreshSet, git};
use crate::tab::RepoTab;

/// Runs on the UI thread, deferred, after a save succeeded (the tab is no longer busy then).
pub type OnSaved = Box<dyn FnOnce(&mut Window, &mut App)>;

impl RepoTab {
    /// ★ Sets (or with `None` unsets) `user.name` / `user.email` in `scope` (`config::set_identity`).
    /// Refresh: CONFIG.
    pub fn save_author(
        &mut self,
        scope: ConfigScope,
        identity: Option<Identity>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.save_authors(vec![(scope, identity)], None, window, cx)
    }

    /// [`save_author`](Self::save_author), then `on_saved` once the save succeeded. The continuation is deferred, so
    /// it may start the next operation (the commit that asked for the identity) on this tab.
    pub fn save_author_then(
        &mut self,
        scope: ConfigScope,
        identity: Option<Identity>,
        on_saved: OnSaved,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.save_authors(vec![(scope, identity)], Some(on_saved), window, cx)
    }

    /// Writes several identities (the Author settings dialog saves global and repository values together) as one
    /// operation. Refresh: CONFIG.
    pub fn save_authors(
        &mut self,
        edits: Vec<(ConfigScope, Option<Identity>)>,
        on_saved: Option<OnSaved>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let spec = OpSpec::new(OpKind::SaveAuthor, move |env| {
            let mut refresh = RefreshSet::empty();
            for (scope, identity) in &edits {
                refresh |= git::config::set_identity(Some(env.repo), *scope, identity.as_ref())?;
            }
            Ok(OpOutcome {
                value: (),
                refresh,
                log: Default::default(),
                warnings: Vec::new(),
            })
        })
        .base_refresh(RefreshSet::CONFIG)
        .then(move |_, _, window, cx| {
            if let Some(on_saved) = on_saved {
                window.defer(cx, on_saved);
            }
        });
        self.run_op(spec, window, cx)
    }

    /// Writes the commit-graph file (`git commit-graph write --reachable --changed-paths`); the log reloads when it
    /// is done. Cancellable.
    pub fn write_commit_graph(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let spec = OpSpec::new(OpKind::WriteCommitGraph, move |env| {
            let (repo, ctx) = (env.repo, env.ctx);
            git::config::write_commit_graph(repo, env.perm(), ctx)
        })
        .cancellable(Cancellable::Yes)
        .base_refresh(RefreshSet::LOG);
        self.run_op(spec, window, cx)
    }
}
