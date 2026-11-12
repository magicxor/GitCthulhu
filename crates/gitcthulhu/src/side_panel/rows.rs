//! Row rendering of the side panel per row kind (owned by G3 after G0b; 07 §6.4, §6.5). Local-branch rows append
//! `forge::badges::branch_row_badge(..)` before "HEAD"; worktree rows render through `worktrees::render_row`.
