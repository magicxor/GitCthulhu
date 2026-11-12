# Rust Instructions

These are defaults for Rust source and test changes under `crates/`; more specific instruction files override
them. Do not apply them to generated, vendored, or fixture data unless the task is specifically about that code.

## Crate Layout

Fixed decisions (owner-approved). The virtual-branch code is gone; the new crates are being built.

- **Git core**: `but-core` (to be slimmed of GitButler-only concepts), `but-error`, `but-serde`, `but-schemars`,
  `but-project-handle`, `but-path`, `but-utils`, `gitbutler-git` (askpass harness and transport defaults used by the `gitcthulhu-git` executor; the
  `gitcthulhu-askpass` and `gitcthulhu-editor` helper programs ship with the `gitcthulhu` package),
  `but-askpass`, `gitbutler-filemonitor`, `but-testsupport`, `but-debugging`.
- **Preserved capabilities**: forge integrations (`but-forge`, `but-forge-storage`, `but-github`, `but-gitlab`,
  `but-bitbucket`, `but-secret`), AI (`but-llm`), embedded terminal (`but-tty`), installer and updater
  (`but-installer`, `but-update`), plus `but-settings` and `but-db`.
- **Planned new crates**: `gitcthulhu-git` (UI-agnostic plain-Git backend, blocking API, no GPUI),
  `gitcthulhu-oplog` (snapshot-based undo/redo on `but_core::snapshot`, no virtual-branch state), and
  `gitcthulhu` (the GUI binary on `gpui-kit` from crates.io; it embeds the fonts in `assets/fonts`
  and the app icons in `assets/app-icon`, still GitButler's artwork until GitCthulhu's own replaces it).
- App data and config identity is `gitcthulhu`. GitButler on-disk state (`.git/gitbutler/*`, `refs/gitbutler/*`,
  `gitbutler.*` config) is neither read nor written.
- Do not reintroduce virtual branches, stacks, workspace commits, API/CLI layers or Tauri/JS tooling.

## Git Engine Policy

- **Reads** (status, diff, log/graph walk, refs, blame, config): in-process via `gix`.
- **Mutations** (commit, checkout, merge, rebase, cherry-pick, revert, reset, stash, tag, branch, remote,
  submodule, worktree, clone, fetch, pull, push): the `git` executable, with askpass-based credential prompts.
  Hooks must run natively; never pass `--no-verify` and surface hook output and failures to the user.
- Whole-file staging, unstaging and discarding run `git add`/`git restore`/`git rm`. Hunk and line staging is an
  in-process index edit (the new blob is the old image plus the selected line edits, written under `index.lock`
  after a stale check); discarding selected lines writes the worktree file back through the filter pipeline.
- No `git2`/libgit2 in new code. Treat existing `git2` use as legacy to be removed.

## Code Shape and Naming

- Use Git domain names and keep types/helpers in the crate or module that owns the concept.
- Avoid vague names like `Manager`, `Service`, `Helper`, `Util`, or `Processor` unless nearby code uses that
  concept.
- Solve the present problem directly. Avoid speculative abstractions: one-use traits/types/functions, fake
  extension points, ceremonial wrappers/modules, and public APIs larger than real callers need.
- Keep module boundaries discoverable from names and the call graph. Avoid drive-by refactors.
- Prefer explicit domain enum matches when wildcard arms would hide behavior.
- Comment public API purpose/invariants/errors and complex algorithm intent; avoid comments that repeat the code.

## Git Repository Semantics

- Do not rediscover Git repositories inside business logic; pass the repository explicitly.
- Preserve Git graph semantics even when the shape looks redundant or odd; do not deduplicate, reorder, or smooth
  graph data unless Git semantics allow it.
- Use `gix` for repository logic.
- Keep Git paths, refnames, commit messages, and diff payloads byte-preserving until the UI boundary; avoid lossy
  `String` conversion in business logic.
- Treat filesystem and worktree writes carefully; use existing locking and access patterns.
- Avoid implicit `SystemTime::now()` in testable business logic; pass time in when deterministic behavior matters.
- Use `anyhow::Context` to explain what operation failed. When callers need classification, use existing
  `but_error::Code` patterns; do not make consumers match error strings.
- New user-facing mutations should participate in undo: snapshot before the mutation and restore on undo. Undo is
  snapshot-based; do not design bespoke inverse operations. Call out any mutation intentionally left out.

## Database Migrations (`but-db`)

- Keep migrations forward-compatible: stay on the current `SchemaVersion` and leave columns or tables that new
  code stopped using in place. Bumping the version locks every older binary out of the database; reserve it for
  planned, coordinated breaks, never routine cleanup.

## Version Control

- Assume the worktree may contain other agents' changes. Do not overwrite, clean up, stage, commit, or amend
  changes you did not make.
- Keep commit messages succinct: a short imperative subject, then why, impact, and core decisions. Do not list
  local validation commands or add AI co-author trailers or tool branding.

## Testing and Validation

- Run the narrowest relevant test or check first, for example `cargo test -p <crate> <test-name>` or
  `cargo check -p <crate> --all-targets`.
- Use `but-testsupport` for fixtures and temporary repositories; prefer read-only fixtures for read-only
  behavior. Backend integration tests run against throw-away repositories created with the `git` executable.
- When snapshot output is volatile, stabilize inputs or normalize output; do not replace strong snapshots with
  vague assertions.
- Run `cargo fmt` for formatting, avoiding unrelated dirty files.
- Run `cargo clippy --fix --allow-dirty` only after inspecting the diff scope and only when generated edits stay
  within the intended change.
- Run `cargo machete` when dependencies changed; verify targets, features, and platform-specific usage first.
- Use workspace-wide checks only when the change affects shared contracts or multiple crates.

## Assertions

- Explain why a standard Rust assertion holds concisely using the last-argument message, like
  `assert!(1!=2, "arithmetic unit on CPU works")`
- Snapshot assertions use `snapbox`, like `snapbox::assert_data_eq!(actual, snapbox::str![[r#"..."#]])`.
  `snapbox::assert_data_eq!` has no message argument, so explain why a snapshot holds with a `// comment` on the
  line above it.
- Regenerate inline snapshots by running the tests with `SNAPSHOTS=overwrite`.
- Remove unstable output before asserting (sanitize ids/timestamps/paths, e.g. the `but_testsupport` helpers)
  rather than snapshotting it raw. `snapbox` also pattern-matches expected data by default (`[..]` and `...`
  wildcards, path-separator normalization); append `.raw()` to the expected `snapbox::str!` when the value must
  match exactly (e.g. it contains backslashes or literal `[..]`/`...`).
