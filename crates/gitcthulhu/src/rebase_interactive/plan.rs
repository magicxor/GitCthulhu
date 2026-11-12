//! Pure edits of a [`RebasePlan`] (no GUI): action rules, reordering, reword drafts and the plan handed to Git.
//!
//! The editor changes the plan in `RepoUi::rebase_plan` only through these functions, so the rules (08 §6, 20
//! §5.6) are unit-testable without a window.

use std::collections::HashMap;

use crate::backend::{ObjectId, RebaseAction, RebasePlan, RebaseStep};

/// The actions offered for the first kept line (nothing before it to meld into).
const FIRST_LINE_ACTIONS: [RebaseAction; 3] =
    [RebaseAction::Pick, RebaseAction::Reword, RebaseAction::Drop];

/// The actions offered for every other line.
const OTHER_LINE_ACTIONS: [RebaseAction; 6] = [
    RebaseAction::Pick,
    RebaseAction::Reword,
    RebaseAction::Squash,
    RebaseAction::Fixup,
    RebaseAction::Edit,
    RebaseAction::Drop,
];

/// The label of an action in the dropdown (08 §6 / §7).
pub fn action_label(action: RebaseAction) -> &'static str {
    match action {
        RebaseAction::Pick => "Pick",
        RebaseAction::Reword => "Reword",
        RebaseAction::Squash => "Squash",
        RebaseAction::Fixup => "Fixup",
        RebaseAction::Edit => "Edit",
        RebaseAction::Drop => "Drop",
    }
}

/// Whether step `ix` is the first one Git keeps (every step before it is dropped), so squash and fixup have no
/// commit to meld into. Gitnuro limits only line 0; a dropped line 0 would let line 1 start with a squash, which
/// Git rejects, so the rule follows the dropped lines.
pub fn is_first_kept(plan: &RebasePlan, ix: usize) -> bool {
    plan.steps
        .iter()
        .take(ix)
        .all(|s| s.action == RebaseAction::Drop)
}

/// The actions the dropdown of step `ix` offers: Pick, Reword, Drop for the first kept line, all six otherwise.
pub fn allowed_actions(plan: &RebasePlan, ix: usize) -> &'static [RebaseAction] {
    if is_first_kept(plan, ix) {
        &FIRST_LINE_ACTIONS
    } else {
        &OTHER_LINE_ACTIONS
    }
}

/// Turns a squash/fixup on the first kept line into a pick (after a drop or a move made it the first one).
pub fn normalise(plan: &mut RebasePlan) {
    if let Some(first) = plan
        .steps
        .iter_mut()
        .find(|s| s.action != RebaseAction::Drop)
        && matches!(first.action, RebaseAction::Squash | RebaseAction::Fixup)
    {
        first.action = RebaseAction::Pick;
    }
}

/// The index of the step of `commit`.
pub fn position(plan: &RebasePlan, commit: ObjectId) -> Option<usize> {
    plan.steps.iter().position(|s| s.commit == commit)
}

/// Sets the action of step `ix`. Leaving Reword parks the edited message in `drafts` (Git would otherwise use it
/// for the commit); choosing Reword again restores it. Returns whether anything changed.
pub fn set_action(
    plan: &mut RebasePlan,
    ix: usize,
    action: RebaseAction,
    drafts: &mut HashMap<ObjectId, String>,
) -> bool {
    if !allowed_actions(plan, ix).contains(&action) {
        return false;
    }
    let Some(step) = plan.steps.get_mut(ix) else {
        return false;
    };
    if step.action == action {
        return false;
    }
    if step.action == RebaseAction::Reword
        && let Some(text) = step.new_message.take()
    {
        drafts.insert(step.commit, text);
    }
    step.action = action;
    if action == RebaseAction::Reword {
        step.new_message = drafts.get(&step.commit).cloned();
    }
    normalise(plan);
    true
}

/// Moves the step at `from` to `to` (the others shift). Returns whether the order changed.
pub fn move_step(plan: &mut RebasePlan, from: usize, to: usize) -> bool {
    let len = plan.steps.len();
    if from >= len || to >= len || from == to {
        return false;
    }
    let step = plan.steps.remove(from);
    plan.steps.insert(to, step);
    normalise(plan);
    true
}

/// The text of a step's message field: the edited message of a Reword step, otherwise the original, without the
/// trailing newline of the raw commit message.
pub fn display_message(step: &RebaseStep) -> &str {
    let text = match (&step.action, &step.new_message) {
        (RebaseAction::Reword, Some(edited)) => edited,
        _ => &step.message,
    };
    text.trim_end_matches(['\n', '\r'])
}

/// Stores the edited message of a Reword step; text equal to the original message is stored as no edit.
pub fn set_message(plan: &mut RebasePlan, ix: usize, text: &str) {
    let Some(step) = plan.steps.get_mut(ix) else {
        return;
    };
    if step.action != RebaseAction::Reword {
        return;
    }
    let original = step.message.trim_end_matches(['\n', '\r']);
    step.new_message = (text != original).then(|| text.to_owned());
}

/// Why the plan cannot be started, if it cannot: nothing to rebase, or a reworded message that is empty (Git
/// aborts on an empty message).
pub fn blocker(plan: &RebasePlan) -> Option<&'static str> {
    if plan.steps.is_empty() {
        return Some("There are no commits to rebase");
    }
    let empty_reword = plan.steps.iter().any(|s| {
        s.action == RebaseAction::Reword
            && s.new_message.as_ref().is_some_and(|m| m.trim().is_empty())
    });
    empty_reword.then_some("A reworded commit message is empty")
}

/// The plan handed to Git: normalised, and only Reword steps carry a message.
pub fn final_plan(plan: &RebasePlan) -> RebasePlan {
    let mut plan = plan.clone();
    normalise(&mut plan);
    for step in &mut plan.steps {
        if step.action != RebaseAction::Reword {
            step.new_message = None;
        }
    }
    plan
}
