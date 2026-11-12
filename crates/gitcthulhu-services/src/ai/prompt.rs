//! The prompts (ported verbatim from the GitButler GUI, 16 A7/A8) and how request data goes into them.

use super::{BranchNameContext, CommitMessageRequest, CommitStyle, PrRequest};

/// System prompt of commit-message generation.
pub(super) const COMMIT_SYSTEM: &str = "You write Git commit messages. Return only the commit message, without Markdown or commentary.";

const COMMIT_INSTRUCTIONS_CONVENTIONAL: &str = "Write a commit message for these changes.
Only respond with the commit message, without notes or Markdown.
Explain what changed and why, focusing on the most important changes.
Use present tense and a semantic commit prefix.
Keep the title under 50 characters and hard-wrap body lines at 72 characters.
Do not use emoji or start lines with the hash symbol.";

const COMMIT_INSTRUCTIONS_PLAIN: &str = "Write a commit message for these changes.
Only respond with the commit message, without notes or Markdown.
Explain what changed and why, focusing on the most important changes.
Use the imperative mood (\"Add user authentication\" instead of \"Adding user authentication\") and no type prefix.
Keep the title under 50 characters and hard-wrap body lines at 72 characters.
Do not use emoji or start lines with the hash symbol.";

/// System prompt of pull-request text generation.
pub(super) const PR_SYSTEM: &str = "You write pull requests.
Return the title on the first line, then a blank line, then the description body as Markdown.
The title is plain text: no leading \"#\", no quotes, no prefix.
Never repeat the title as a heading or first line of the body — it is shown above the body already.
Return nothing else: no commentary, no surrounding code fence.";

/// System prompt of branch-name suggestions.
pub(super) const BRANCH_SYSTEM: &str = "You name Git branches. Return only the branch name: lowercase kebab-case, at most 5 words, optional `feat/`, `fix/`, `chore/` prefix, no quotes, no commentary.";

/// How much commit-message history goes to the model.
const HISTORY_LIMIT: usize = 5_000;

/// The user prompt asking for a commit message of `req.patch`, with what the author already wrote as notes.
///
/// The patch text is already budgeted by `gitcthulhu_git::diff::staged_patch_text`: it opens with the list of
/// every changed file and ends with a `[diff truncated: N more files]` marker when files were cut.
pub(super) fn commit_prompt(req: &CommitMessageRequest) -> String {
    let instructions = match req.style {
        CommitStyle::Conventional => COMMIT_INSTRUCTIONS_CONVENTIONAL,
        CommitStyle::PlainImperative => COMMIT_INSTRUCTIONS_PLAIN,
    };
    let notes = match req.notes.trim() {
        "" => String::new(),
        notes => format!("\n\nThe author's notes on it:\n```\n{notes}\n```"),
    };
    format!(
        "{instructions}{notes}\n\nSelected changes:\n```diff\n{}\n```",
        req.patch.text.trim_end()
    )
}

/// The user prompt asking for a pull request's title and description, from its commit messages (oldest first)
/// and what is written so far.
pub(super) fn pr_prompt(req: &PrRequest) -> String {
    let messages = req.commit_messages.join("\n\n---\n\n");
    let mut sections = vec![
        "Write the title and description for this pull request.".to_owned(),
        "List the most important changes. Use bullet points. Be concise.".to_owned(),
    ];
    if !req.title.trim().is_empty() {
        sections.push(format!("Working title:\n```\n{}\n```", req.title.trim()));
    }
    if !req.body.trim().is_empty() {
        sections.push(format!(
            "Description so far, to build on:\n```\n{}\n```",
            req.body.trim()
        ));
    }
    let (messages, cut) = clipped(&messages, HISTORY_LIMIT);
    let marker = if cut {
        "\n[commit messages truncated]"
    } else {
        ""
    };
    sections.push(format!("Commit messages:\n```\n{messages}{marker}\n```"));
    sections.join("\n\n")
}

/// The user prompt asking for a branch name.
pub(super) fn branch_prompt(ctx: &BranchNameContext) -> String {
    match ctx {
        BranchNameContext::Staged(patch) => format!(
            "Suggest a branch name for work that includes these changes.\n\n```diff\n{}\n```",
            patch.text.trim_end()
        ),
        BranchNameContext::CommitMessage(message) => format!(
            "Suggest a branch name for work that starts from this commit.\n\nCommit message:\n```\n{}\n```",
            clipped(message.trim(), HISTORY_LIMIT).0
        ),
    }
}

/// The first `limit` characters of `text`, and whether anything was cut.
fn clipped(text: &str, limit: usize) -> (&str, bool) {
    match text.char_indices().nth(limit) {
        Some((at, _)) => (text.get(..at).unwrap_or(text), true),
        None => (text, false),
    }
}
