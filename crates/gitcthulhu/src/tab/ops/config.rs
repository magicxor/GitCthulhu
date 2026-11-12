//! Configuration operations of the catalogue (spec 21 §2.11; owned by G10 after G0b). G0b implements the reference
//! method `save_author` (a backend call without permission or context, wrapped into an `OpOutcome`).

use gpui_kit::{Context, Window};

use super::{OpKind, OpSpec};
use crate::backend::{ConfigScope, Identity, OpOutcome, RefreshSet, git};
use crate::tab::RepoTab;

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
        let spec = OpSpec::new(OpKind::SaveAuthor, move |env| {
            let refresh = git::config::set_identity(Some(env.repo), scope, identity.as_ref())?;
            Ok(OpOutcome {
                value: (),
                refresh,
                log: Default::default(),
                warnings: Vec::new(),
            })
        })
        .base_refresh(RefreshSet::CONFIG);
        self.run_op(spec, window, cx)
    }

    /// Writes the commit-graph file (`config::write_commit_graph`). Owner: G10.
    pub fn write_commit_graph(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.not_implemented("Write commit-graph", cx);
        false
    }
}
