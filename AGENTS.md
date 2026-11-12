# GitCthulhu Agent Instructions

GitCthulhu is a plain Git GUI client written in Rust: a fork of GitButler with virtual branches, stacks and the
Tauri/JS front end removed. The GUI is built on `gpui-kit` with a Gitnuro-like layout; the long-term feature set
follows GitKraken. There are no virtual branches, no workspace commit and no non-standard Git behaviour.

Apply all relevant instruction files. If instructions conflict, resolve them in this order:

1. Explicit human instructions
2. Nearest nested `AGENTS.md`
3. This file

## Repo Map

- `crates/` - all code is Rust; see `crates/AGENTS.md` for the crate layout and Rust rules.
- `scripts/` - helper scripts for the Rust workspace and the installer bootstrap.
- `.github/` - one Rust CI workflow (Windows, Linux, macOS) plus security scans.

## Working Style

- Treat questions about the codebase as read-only unless the user asks for changes.
- Make focused, reviewable changes; avoid unrelated rewrites.
- Use the simplest design that solves the actual problem; do not add speculative machinery, and remove machinery
  your change makes unnecessary.
- Inspect nearby code before introducing patterns. Prefer existing APIs, tests, and conventions.
- Run targeted validation for the area touched.
- Before adding new machinery to fix a behavior bug, reproduce the bug in a failing test and survey the target
  file's existing loops and classifications as candidate hosts.
- When a fix calls for a new mechanism (a new module, a new public API, or a parallel walk where one already
  exists), propose the intended shape before building it.

## Build Prerequisites

- Rust as pinned in `rust-toolchain.toml`.
- Windows: the Windows SDK (`fxc.exe`). Linux GUI builds: libxcb, xkbcommon-x11 and fontconfig dev packages.
- `but-tty` builds libghostty-vt and needs Zig 0.17 on `PATH`.

## Scoped Instructions

- For Rust work under `crates/`, follow `crates/AGENTS.md`.

## Building and testing for agents

Several agents usually work at once, each in its own git worktree outside the main checkout
(`C:\git\opensource\GitCthulhu.wt\<id>`), on one machine. Keep builds incremental and narrow, leave nothing behind.

- **Fresh worktree:** before the first cargo command run `pwsh -NoProfile -File tools/dev/seed-target.ps1`. It copies the
  main checkout's compiled dependencies into this worktree's `target/`, so only the workspace crates are compiled
  (minutes saved). The main checkout's `target/` is the seed: after a `Cargo.lock`, toolchain or profile change,
  rebuild it there (`cargo test --no-run` / `cargo check --all-targets` for the main crates) before creating worktrees.
- **While editing** use `cargo check` / `cargo clippy -p <crate> --all-targets` (no code generation, no linking; dev
  builds are incremental); run `cargo test` once the change is done. Narrow every build: `-p <crate>`, `--lib`, `--test
  <name>`. Do not build `--release` unless a spec's perf test needs it.
- **Never** run `cargo clean` in the main checkout (it deletes every worktree's seed) and never share one
  `CARGO_TARGET_DIR` between worktrees (cargo's build lock would serialize their builds).
- **Linux** only for platform-specific changes (`cfg(unix)`/`cfg(windows)`/`target_os`, file permissions, libc,
  `Cargo.toml`/`Cargo.lock`; CI covers the rest), and only through `pwsh -NoProfile -File tools/dev/linux-test.ps1
  <cargo arguments>` (Docker; `--git-min` runs the Git 2.34.1 image for the backend suite). Once per session first:
  `... linux-test.ps1 --update`. When the worktree is done: `... linux-test.ps1 --drop-volume`. Never `docker run` a Rust
  image with the build inside the container.
- **Git Bash** rewrites POSIX-looking arguments for Windows programs (`/work` given to docker becomes
  `C:/Program Files/Git/work`): prefix exactly that one command with `MSYS_NO_PATHCONV=1`, never `export` it, and give
  Windows programs `C:/...` paths (with it set, `/c/...` is not converted and means `C:\c\...`).
- **Docker objects** an agent creates are named `gitcthulhu-tmp-<task>-<what>`, never generic (`test`, `app`), and are
  removed at the end of the task: only your own, never `docker system prune`, `docker image prune -a` or `docker volume
  prune`. The tools' own objects are named `gitcthulhu-*`. **Permanent, never delete:** the volumes
  `gitcthulhu-target-main-<hash>` and `gitcthulhu-target-gitmin-main-<hash>` (seeds of the worktree volumes),
  `gitcthulhu-cargo-home`, the images `gitcthulhu-linux-dev:*` (`--prune-images` drops stale tags, only while nobody else
  starts a Linux build) and the main checkout's `target/`. A worktree's volume `gitcthulhu-target-[gitmin-]wt-<folder>-<hash>`
  goes only through `--drop-volume` or `--gc`.
- Temporary files go only into your scratch directory, never into the repo.
- Never inject OS input (SendKeys, synthetic mouse or keyboard, desktop UI automation): keystrokes land in whatever
  window has the focus. Do not change system settings or stop processes you did not start.
- Timings are noisy: parallel agents keep the machine busy and timings vary up to 4x. Never loosen a timeout or perf
  threshold to get green; rerun alone and judge by medians.
- Dev/test builds carry line tables only for workspace crates and no debug info for dependencies (`[profile.dev]` in
  `Cargo.toml`).

## gpui-kit pitfalls (0.7.3, Windows)

- gpui-kit is pinned (`=0.7.3`) and the GUI relies on its internals: check APIs against the sources in the cargo
  registry, not a gpui-kit git checkout (it drifts ahead of the release).
- Platform side effects (clipboard, dialogs, spawning processes, opening URLs, revealing paths) panic on the gpui test
  platform: route them through an injectable services object and record them in tests.
- `cx.set_menus` renders nothing on Windows: use `AppMenuBar` inside the `TitleBar` (`set_menus` stays for macOS).
  `window.prompt` is light-themed in dark mode: use in-app dialogs.
- `is_action_available` before a window's first draw can panic (no dispatch tree yet): sync menu availability after the
  first frame.
- A focused `Input` consumes `ctrl-x` (even with no selection), `home`/`end` and editing chords: shortcuts that must work
  while typing are bound again with a `"<Context> > Input"` predicate; scope single-letter shortcuts to non-input
  contexts. Parse user shortcuts with `KeyBinding::load` (`KeyBinding::new` panics on bad input).
- A root `track_focus` steals the focus from an `Input` on any click; `TextView` takes the focus on mouse-down and loses
  it when its row scrolls out of a virtualized list: refocus deliberately.
- Lists: `uniform_list` for uniform rows, `gpui_kit::list` with `ListState` for variable heights. Never `measure_all()`
  (seconds to first paint, ~1 GB at a few thousand rows); cap entity-backed per-row state with an LRU; keep the scroll
  anchor by diffing item ids into `ListState::splice`.
- `combine_highlights` picks the winner of two colors in hash order: where two layers set the same field, split the runs
  by hand.
- The focused input's caret blink repaints about twice a second: idle-CPU and idle-frame checks must account for it.
