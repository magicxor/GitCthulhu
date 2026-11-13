# Developing GitCthulhu

GitCthulhu is one Rust workspace (`crates/`). [`crates/AGENTS.md`](crates/AGENTS.md) holds the engineering rules;
[`AGENTS.md`](AGENTS.md) the rules for agents working on this checkout.

## Workspace layout

| Group | Crates |
| --- | --- |
| GUI | `gitcthulhu`: the binary (gpui-kit 0.7.3, pinned), plus the helper programs `gitcthulhu-askpass` and `gitcthulhu-editor` |
| Git backend | `gitcthulhu-git`: plain-Git backend, blocking API, no GPUI; `gitcthulhu-oplog`: snapshot undo/redo |
| Services | `gitcthulhu-services`: forge, AI and update services over the crates below, no GPUI |
| Preserved from GitButler | `but-forge`, `but-forge-storage`, `but-github`, `but-gitlab`, `but-bitbucket`, `but-secret` (forges and keychain), `but-llm` (AI), `but-tty` (terminal emulation, libghostty-vt), `but-update`, `but-installer`, `but-db` |
| Git core and support | `but-core` (snapshots for the oplog; GitButler-era tests with known failures), `gitbutler-git` (askpass harness), `gitbutler-filemonitor` (+ `gitbutler-notify-debouncer`), `but-error`, `but-serde`, `but-utils`, `but-path`, `but-project-handle`, `but-askpass`, `but-testsupport` |

`but-path`, `but-project-handle`, `but-askpass` and `but-installer` have no dependents in the workspace.

## Git engine policy

- Reads (status, diff, log and graph walk, refs, blame, file history, config) run in-process through `gix`.
- Mutations (commit, checkout, merge, rebase, cherry-pick, revert, reset, stash, tag, branch, remote, submodule,
  worktree, clone, fetch, pull, push) run the `git` executable (2.34.1 or newer) with askpass-based credential
  prompts, so hooks run natively; `--no-verify` is only passed for an explicit "Retry without hooks".
- Whole-file staging runs `git add` / `git restore` / `git rm`; hunk and line staging is an in-process index edit
  under `index.lock` after a stale check.
- No `git2`/libgit2.

## Prerequisites

- Rust, as pinned in `rust-toolchain.toml` (install with rustup).
- Windows: the Windows SDK (provides `fxc.exe`).
- Zig 0.17 on `PATH`: `but-tty` builds libghostty-vt with it, and its build script clones the Ghostty sources at the
  pinned commit (network and `git` needed on the first build; `GHOSTTY_SOURCE_DIR` points it at a local checkout
  instead). The GUI's default `terminal` feature depends on `but-tty`, so `--exclude but-tty` alone does not help.
  Without Zig use `cargo check --workspace --exclude but-tty --exclude gitcthulhu` plus `cargo check -p gitcthulhu
  --no-default-features --features services` for the GUI; the Terminal button is then disabled.
- Linux: `libxcb1-dev libx11-xcb-dev libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libfontconfig-dev
  libfreetype-dev libssl-dev pkg-config`. CI and the Docker image also install `libdbus-1-dev`
  (`scripts/install-minimal-debian-dependencies.sh`), although no crate of the current dependency graph links it.
- Optional: `cargo-nextest` (`make test`); `nix develop` gives Rust, Zig, nextest, cargo-deny and a few tools, but
  not the Linux GUI libraries above.

## Everyday commands

```sh
make            # fmt, fmt-check, check, clippy (the whole workspace: needs Zig)
make test       # cargo nextest run --workspace --exclude but-core
cargo test --workspace --exclude but-core
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt
```

`but-core` still carries GitButler-era tests with known failures (on Windows its fixtures need Unix shell scripts;
on Linux three ref-name tests fail), so the commands above exclude it. CI runs it separately and non-blocking:
`cargo test -p but-core` (`make test-legacy`).

`scripts/cargo-test.sh` deletes generated fixtures before running tests. `scripts/clean.sh` deletes the generated
fixtures, and with `--target` also `target/` (never in a main checkout that seeds agent worktrees).

CI (`.github/workflows/ci.yaml`): fmt; clippy, check, the terminal-free GUI check and the tests on Windows, Linux and
macOS; the `gitcthulhu-git` suite in an Ubuntu 22.04 container with Git 2.34.1 and a German locale. It has not run
yet for this branch (never pushed).

## Build tooling for parallel worktrees

- `tools/dev/seed-target.ps1` copies the main checkout's compiled dependencies into a fresh worktree's `target/`, so
  only the workspace crates are compiled there. The main checkout's `target/` is the seed: never `cargo clean` it.
- `.cargo/config.toml` sets `--cfg tokio_unstable` for Tokio diagnostics; `[profile.dev]` keeps line tables only for
  the workspace crates.

## Linux runner (Docker)

`pwsh -NoProfile -File tools/dev/linux-test.ps1 <cargo arguments>` runs cargo for this checkout in a container
(image `gitcthulhu-linux-dev:<toolchain>-<hash>`, rust:1-trixie with the Linux packages and Zig, user uid 1000). Build
state lives in named volumes: `gitcthulhu-target-main-<hash>` for the main checkout,
`gitcthulhu-target-wt-<folder>-<hash>` for a worktree, and the shared `gitcthulhu-cargo-home`. `--git-min` uses a
second image built from ubuntu:22.04 (Git 2.34.1) with its own volumes. `--update` refreshes the image,
`--drop-volume` deletes a worktree's volume, `--gc` deletes the volumes of worktrees that no longer exist,
`--prune-images` deletes other tags of the image. The script header documents every option. Examples:

```powershell
pwsh -NoProfile -File tools/dev/linux-test.ps1 test --workspace --exclude but-core
pwsh -NoProfile -File tools/dev/linux-test.ps1 --git-min test -p gitcthulhu-git
```

## QA runner: screenshots of the real window (Windows)

Headless gpui tests (`#[gpui_kit::test]`) check element bounds against the layout specs. To see the real window, the
`qa` feature adds an in-process scenario runner (`crates/gitcthulhu/src/qa`, `tools/qa`): the app drives itself
through its own actions, waits until it is idle and captures its own window with `PrintWindow` into PNG files. No OS
input is injected. Screen capture is implemented for Windows only.

```powershell
pwsh -NoProfile -File tools\qa\new-sandbox.ps1 -Dest <a folder below "scratch" or %TEMP%>\sbx
pwsh -NoProfile -File tools\qa\run-gui.ps1 -Sandbox <sbx> -Scenario tools\qa\scenarios\main-window.qa
pwsh -NoProfile -File tools\qa\run-credential-flow.ps1 -Sandbox <sbx>    # push through a local http server
```

The sandbox holds synthetic fixture repositories and the app data of the run; a qa build refuses to start outside
one (no network, no real profile). `run-gui.ps1` builds with `--features qa`, writes `report.txt` (one PASS/FAIL/SKIP
line per step), `verbs.txt` (the scenario language of the build) and the screenshots into `<sbx>\out\<scenario>`.
Exit codes: 0 passed, 1 a step failed, 2 usage error, 3 the sandbox gate refused, 101 panic (`panic.txt`), 124
timeout, 125 no report. The scenarios in `tools/qa/scenarios` cover the welcome page, the main window, dialogs,
rebase, undo, terminal, AI, forge, worktrees, update notice and credentials. `AGENTS.md` describes the scenario
language and how to add verbs. Never run a qa build outside `run-gui.ps1`.

## Performance tests

The budgets are in spec 20 §9 (backend) and 21 §4.1 (GUI). The perf tests are `#[ignore]`d; they print their
measurements and assert the budget only with `GITCTHULHU_PERF_ASSERT=1`. Run them in release, one at a time, on a
quiet machine, and judge by the median of several runs:

```sh
GITCTHULHU_PERF_ASSERT=1 cargo test --release -p gitcthulhu-git --test perf -- --ignored --exact perf_repo_open --nocapture
GITCTHULHU_PERF_ASSERT=1 cargo test --release -p gitcthulhu-git --test status -- --ignored perf_status_100k_files --nocapture
cargo test --release -p gitcthulhu-oplog --test restore -- --ignored perf --nocapture
cargo test --release -p gitcthulhu --lib diff_50k -- --ignored
```

`tests/perf.rs` holds the log, refs, commit details, blame and file history tests (sizes overridable through
`GITCTHULHU_PERF_*`, see its header); status, graph, diff and stage have theirs next to their modules. CI does not
run them.

## Debugging

- Logs honour `RUST_LOG` and go to the log file below. A Windows release build has no console
  (`windows_subsystem = "windows"`), so output only reaches the log file there.
- `GITCTHULHU_HOME` redirects all application directories (the tests and the QA sandbox use it).

## Where GitCthulhu keeps its data

| What | Location |
| --- | --- |
| Config: `settings.json`, `state.json`, `repos.json` | `dirs::config_dir()/gitcthulhu` (`%APPDATA%\gitcthulhu`, `~/.config/gitcthulhu`, `~/Library/Application Support/gitcthulhu`) |
| Data (forge storage, ...) | `dirs::data_local_dir()/gitcthulhu` (`%LOCALAPPDATA%\gitcthulhu`, `~/.local/share/gitcthulhu`, `~/Library/Application Support/gitcthulhu`) |
| Logs: `gitcthulhu.log`, rotated at 10 MB, 5 files kept | `<data>/logs` |
| Cache | `dirs::cache_dir()/gitcthulhu` (`%LOCALAPPDATA%\gitcthulhu`, `~/.cache/gitcthulhu`, `~/Library/Caches/gitcthulhu`) |

On Windows data and cache are the same folder. `GITCTHULHU_HOME` replaces all of them by `config`, `data`, `logs`
and `cache` below it. Attach `gitcthulhu.log` to bug reports.

- **Inside your repositories**: the undo log is kept as Git objects under `refs/gitcthulhu/oplog/main` (a linked
  worktree uses `refs/gitcthulhu/oplog/linked/<id>`), and the commit message draft in
  `.git/gitcthulhu/commit-draft`. Inspect the refs with `git for-each-ref refs/gitcthulhu`; remove one with `git
  update-ref -d <ref>` (this drops the undo history of that repository; `git gc` then frees the objects).
- **OS keychain**: forge tokens, AI provider keys and the proxy password are stored in the OS keychain (service
  names start with `gitcthulhu-`, user name `GitCthulhu`), never in the files above.
- **Reset**: close the application and delete the directories above. To remove everything, also delete the
  keychain entries and the `refs/gitcthulhu` refs.

## Build configuration

Compile-time variables are read with `option_env!`/`env!`, so they must be set in the environment of the
`cargo build` process; changing one needs a rebuild.

| Variable | Effect |
| --- | --- |
| `GITCTHULHU_GITHUB_CLIENT_ID` | GitHub OAuth app client id. Unset or empty: GitHub device-flow sign-in is disabled. |
| `GITCTHULHU_UPDATE_MANIFEST_URL` | Update manifest URL (`{channel}` is replaced by `release` or `nightly`). Unset or empty: update checks and the update notice are disabled. |
| `GITCTHULHU_CHANNEL` | Icon set of the Windows executable (`assets/app-icon/<channel>`, default `release`). |

A release build needs the first two set; without them it silently lacks GitHub sign-in and update checks.

Features of the `gitcthulhu` crate:

| Feature | Meaning |
| --- | --- |
| `terminal` (default) | Embedded terminal (`but-tty`; needs Zig 0.17) |
| `services` (default) | Forge, AI and update services; without it `start_services` fails with "built without integrations" |
| `qa` | In-process QA scenario runner and screenshots (`tools/qa`); never in releases |
| `test-support` | gpui-kit test support, tests only |
| `gpui-fast` | gpui-kit benchmark passthrough only |

## Releasing

`.github/workflows/release.yaml` builds and publishes a release when a version tag is pushed.

1. Set the new version in `crates/gitcthulhu/Cargo.toml` (run `cargo check -p gitcthulhu` so `Cargo.lock` follows) and
   merge that to `master`. The application shows this version, so the workflow refuses a tag that differs from it.
2. Tag that commit with the version itself, a [Semantic Versioning 2.0.0](https://semver.org) version without a
   leading `v`, and push the tag: `git tag 0.2.0 && git push origin 0.2.0`. A version with a pre-release part
   (`0.2.0-rc.1`) is published as a pre-release.

The workflow builds `cargo build --release --locked -p gitcthulhu` on Windows (x86_64), Linux (x86_64, on the oldest
Ubuntu runner, so the binaries need glibc 2.35 or newer) and macOS (Apple silicon), and publishes a GitHub release
with generated notes, a `SHA256SUMS` file and one archive per platform:

| Archive | Contents |
| --- | --- |
| `gitcthulhu-<version>-windows-x86_64.zip` | `gitcthulhu.exe` with its helpers `gitcthulhu-askpass.exe` and `gitcthulhu-editor.exe`, which must stay next to it |
| `gitcthulhu-<version>-linux-x86_64.tar.gz` | the same three binaries |
| `gitcthulhu-<version>-macos-aarch64.tar.gz` | `GitCthulhu.app`, ad-hoc signed |

The macOS build is optional: when it fails or GitHub has no macOS runner to give, the release is published with
the Windows and Linux archives only and says so in its notes. To add it later, re-run the macOS job from the
run's page: `publish` then runs again and uploads the archive to the same release (the sentence in the release
notes has to be removed by hand).

Nothing is code-signed: Windows SmartScreen warns, and macOS Gatekeeper blocks a downloaded copy until the user
allows it (System Settings > Privacy & Security, or `xattr -dr com.apple.quarantine GitCthulhu.app`).

The compile-time variables of the section above are taken from the repository's Actions *variables*
(`GITCTHULHU_GITHUB_CLIENT_ID`, `GITCTHULHU_UPDATE_MANIFEST_URL`); unset, the release has no GitHub device-flow
sign-in and no update checks. A pull request that changes the workflow file runs it as a dry run: everything is
built and packaged, the archives are kept as workflow artifacts for a week, and nothing is published.

## Installer and updater

`but-installer` (with `scripts/install.sh`) and `but-update` have no built-in endpoints. The installer needs
`GITCTHULHU_RELEASE_URL` (release API base URL) and `GITCTHULHU_RELEASE_PUBLIC_KEY` (minisign key of the releases),
the bootstrap script additionally `GITCTHULHU_INSTALLER_URL`; release information is only accepted from the
configured host, downloads also from its subdomains, always over HTTPS. The macOS installer expects a
`GitCthulhu.app` bundle with `Contents/MacOS/{gitcthulhu,gitcthulhu-askpass,gitcthulhu-editor}`, the Linux one a
`gitcthulhu` binary (plus `gitcthulhu.sig`) next to the platform download. The release workflow publishes the
application archives only: the release information, the signatures and the installer binary these two need are
not published yet, and the bootstrap script downloads the installer without a signature check.
