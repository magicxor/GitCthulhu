# GitCthulhu

GitCthulhu is a desktop Git client that works with ordinary Git: branches, the index, the working tree and remotes,
nothing else. It is a fork of [GitButler](https://github.com/gitbutlerapp/gitbutler) with virtual branches, stacks,
the workspace commit and the web/Tauri front end removed. The GUI is written in Rust on
[gpui-kit](https://crates.io/crates/gpui-kit); its control layout follows the Git client Gitnuro.

Git reads (status, diff, log and graph, refs, blame, file history, config) run in-process through `gix`. Every
mutation runs the `git` executable, so hooks, signing, credential helpers, filters and LFS behave as they do on the
command line. GitCthulhu never passes `--no-verify` unless you choose "Retry without hooks" after a hook failed.

## Features

- **Repositories**: several repositories in tabs (restored on the next start), a welcome page with recent
  repositories, open, clone (with progress and credential prompts) and "Start a local repository" (`git init`).
- **Log**: commit graph with branch, tag and remote chips, search, uncommitted-changes row, commit details with the
  changed files, and per-commit actions: checkout, create branch or tag, cherry-pick, revert, reset (soft, mixed,
  hard), interactive rebase from a commit.
- **Side panel**: local branches, remotes and remote branches, tags, stashes, submodules and linked worktrees, with a
  filter. Branch actions include checkout, merge, rebase, rename, delete, upstream, push and pull; stash apply, pop
  and drop; submodule add, init, update, sync and delete; worktree add, open in a tab, lock, unlock, remove and prune.
- **Status and commit**: staged and unstaged lists (flat or tree), stage and unstage whole files, hunks or single
  lines, discard, commit, amend, sign-off and author override, a commit message draft that survives restarts, and
  merge, cherry-pick, revert and rebase states with continue, skip and abort.
- **Diff**: unified and split views with syntax highlighting, image, binary and submodule diffs, blame and file
  history.
- **Remotes**: fetch, pull (merge or rebase), push, force push, push tags, remote management, and username/password
  dialogs that answer Git's askpass prompts. Proxy settings are passed to Git.
- **Undo and redo**: before each mutating operation the app records a snapshot (under `refs/gitcthulhu/oplog/*`);
  Undo, Redo and a history popover restore earlier states. Untracked files that an operation would delete are
  captured within a size budget; when they cannot be captured the app asks before deleting them.
- **Embedded terminal** (optional `terminal` feature): a panel at the bottom of each repository tab (libghostty-vt).
- **AI**: commit message, branch name and pull-request text generation through OpenAI, Anthropic, Ollama, LM Studio
  or OpenRouter. Hosted providers are only contacted after a one-time confirmation per provider and host.
- **Forges**: GitHub, GitLab and Bitbucket accounts (tokens in the OS keychain), pull-request and CI badges on
  branches, "Create pull request" and "Checkout pull request".
- **Update notice**: checks an update manifest and shows a notice in the bottom bar (only when the build is
  configured with a manifest URL; see below). There is no in-app installation.
- **Settings**: dark and light theme, scale, date and time format, layout, proxy, authentication, accounts, AI,
  terminal, updates and logs.

## Building

Requirements:

- Rust, as pinned in [`rust-toolchain.toml`](rust-toolchain.toml) (install with rustup).
- Windows: the Windows SDK (it provides `fxc.exe`, which the GUI build needs).
- [Zig 0.17](https://ziglang.org/download/) on `PATH` for the embedded terminal (`but-tty` builds libghostty-vt with
  it). The first build with the terminal also downloads the Ghostty sources, so it needs network access and `git`.
- Linux: the development packages listed in [DEVELOPMENT.md](DEVELOPMENT.md#prerequisites).
- At runtime: a `git` executable on `PATH`, version 2.34.1 or newer.

With Zig:

```sh
cargo build -p gitcthulhu              # debug build, with the terminal
cargo build --release -p gitcthulhu    # release build
```

Without Zig, build the GUI without the terminal (the Terminal button is then disabled):

```sh
cargo build -p gitcthulhu --no-default-features --features services
```

Workspace-wide commands without Zig need both `--exclude but-tty` and `--exclude gitcthulhu`, because the GUI's
default `terminal` feature pulls `but-tty` back in.

A build has no GitHub sign-in and no update checks unless `GITCTHULHU_GITHUB_CLIENT_ID` and
`GITCTHULHU_UPDATE_MANIFEST_URL` are set at build time ([DEVELOPMENT.md](DEVELOPMENT.md#build-configuration)).

## Running

```sh
cargo run -p gitcthulhu [-- <repository directory>]
```

A release build puts `gitcthulhu`, `gitcthulhu-askpass` (answers Git's credential prompts) and `gitcthulhu-editor`
(the editor Git starts for commit messages and rebase todo lists) into `target/release`. The three programs must stay
in the same directory. A redistributed package should also carry the font licences,
`crates/gitcthulhu/assets/fonts/LICENSE-*.txt` (the About dialog shows them too).

Settings, window state and logs live under the `gitcthulhu` application directories (`%APPDATA%\gitcthulhu` and
`%LOCALAPPDATA%\gitcthulhu` on Windows, `~/.config/gitcthulhu` and `~/.local/share/gitcthulhu` on Linux); the undo
history lives in each repository under `refs/gitcthulhu/`, and tokens and keys in the OS keychain. Details and how to
reset everything: [DEVELOPMENT.md](DEVELOPMENT.md#where-gitcthulhu-keeps-its-data).

## Testing

```sh
cargo test --workspace --exclude but-core
cargo test -p gitcthulhu --no-default-features --features services   # the GUI without the terminal
```

`but-core` is inherited from GitButler and still carries tests with known failures (several on Windows, where its
fixtures need Unix shell scripts, and three on Linux); CI runs it separately and non-blocking (`cargo test -p
but-core`). Performance tests, the QA runner for real-window screenshots and the Linux Docker runner are described in
[DEVELOPMENT.md](DEVELOPMENT.md).

## Known limitations

- Developed and tested on Windows 11; the Linux test suite also runs headless in Docker. macOS is built and tested
  only by CI, where a complete green run has not been observed yet, and the real window has never been opened on
  Linux or macOS.
- Releases are built by CI from a version tag ([DEVELOPMENT.md](DEVELOPMENT.md#releasing)) and are not code-signed:
  Windows SmartScreen and macOS Gatekeeper warn about them. There is no installer endpoint or update manifest yet:
  `but-installer` and `but-update` refuse to run until release URLs and a signing key are configured.
- The GitHub, GitLab and Bitbucket integrations were only tested against fake local servers; real endpoints and the
  real GitHub device flow have not been exercised.
- Manual checks are still open: interactive shells (PowerShell 7, Git Bash, vim) in the terminal panel, IME and
  AltGr input. The window was only checked through capture-only screenshots.
- Undo restores snapshots of the repository; it cannot undo what happened on a remote (a push).

## License

See [LICENSE.md](LICENSE.md). Code inherited from GitButler remains under its Functional Source License terms.
