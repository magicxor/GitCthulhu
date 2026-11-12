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
- `but-tty` builds libghostty-vt and needs Zig 0.17 on `PATH`. The `gitcthulhu` crate's default features include
  `terminal` (which depends on `but-tty`), so building the GUI needs Zig too; on a machine without Zig use
  `cargo check -p gitcthulhu --no-default-features --features services` (agents: a portable Zig is at
  `C:\git\opensource\GitCthulhu.plan\tools\zig`, prepend it to `PATH` for the build command).

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

## QA scenarios: looking at the real window (feature `qa`)

Headless gpui tests do not catch everything (a crash on startup, a layout that differs from the Gitnuro specs). The
sanctioned way to look at the REAL window is the in-process QA runner of the `qa` feature (`crates/gitcthulhu/src/qa`,
`tools/qa`). It drives the app through its own actions and entry points (never OS input), waits until the app is idle
and writes PNG screenshots of the app's own window by capture only (`PrintWindow` of its HWND: nothing is clicked,
typed, moved or focused).

```powershell
# once per machine/session: a sandbox with synthetic fixture repositories (below a "scratch" folder or %TEMP%)
pwsh -NoProfile -File tools\qa\new-sandbox.ps1 -Dest C:\git\opensource\GitCthulhu.plan\scratch\<task>\sbx
# one scenario: builds with --features qa, runs the app, prints the report and the screenshot paths
pwsh -NoProfile -File tools\qa\run-gui.ps1 -Sandbox <sbx> -Scenario tools\qa\scenarios\main-window.qa
```

Then `Read` the PNG files (the output directory defaults to `<sbx>\out\<scenario>`; `report.txt` has one PASS/FAIL/SKIP
line per step, `verbs.txt` lists every verb of the build). The window opens briefly on the desktop at 1280x720 and
does not take the keyboard focus; keep runs short and few (one scenario per question, several `shot` steps per run).
Exit codes: 0 passed, 1 a step failed, 2 usage error, 3 the gate refused, 124 timeout (the app's watchdog, or the
script kills the process it started 15 s later), 125 no report (crash: see `stderr.txt`, `panic.txt`). A qa build
only runs scenarios: it never opens an interactive window.

- **Scenario language** (`tools/qa/scenarios/*.qa`, one step per line, `#` comments): `open repository <fixture|path>`,
  `select tab|log row|commit|branch|tag|stash|file|uncommitted row ...`, `open diff`, `set diff split|unified`,
  `toggle diff`, `open dialog <name>`, `open context menu <target>`, `open settings page <name>`, `set theme
  dark|light`, `resize WxH`, `key`/`type`/`action`, `begin action <Name>` (no settle: for an operation that waits
  for a prompt), `wait idle`, `wait dialog open|closed`, `shot <name>`, `assert no error`, `quit`. Fixtures:
  `linear`, `rich` (branches, merge, tags, stashes, local bare remote, ahead/behind, a merge in progress with staged,
  unstaged, untracked and conflicted files), `rebase` (rebase stopped by a conflict) and `cred` (one commit ahead of
  a bare remote that only `-HttpServer` makes reachable).
- **Credential flow (no real network):** `pwsh -NoProfile -File tools/qa/run-credential-flow.ps1 -Sandbox <sbx>` runs
  `scenarios/m2-credentials.qa` through `run-gui.ps1 -HttpServer`: `tools/qa/git-http-server.ps1` serves the bare
  remote on a free `127.0.0.1` port through `git http-backend` and answers 401 until it gets `qa-user` / `qa-secret`;
  the push makes Git call the askpass helper, the app shows the credential dialogs, the script types the answers and
  then checks the remote's head, the server's request log and that no output contains the password. Only for this
  run the gate accepts `GIT_ALLOW_PROTOCOL=file:http` together with `GITCTHULHU_QA_LOOPBACK_HTTP=127.0.0.1:<port>`.
- **Extending** (regions): add verbs with `crate::qa::register_verb(cx, Verb::new(name, usage, help, handler))` from the
  region's `register(cx)` under `#[cfg(feature = "qa")]`, and idle conditions with `crate::qa::register_idle_probe`
  (see `qa/verbs.rs`). A handler runs on the UI thread and calls the region's public entry points.
- **The sandbox gate:** a qa build refuses to start (exit 3) unless `GITCTHULHU_QA_SANDBOX` names a directory with the
  marker written by `new-sandbox.ps1` (never a drive root or the real profile directory or above it), `GITCTHULHU_HOME`, the profile variables and `GIT_CONFIG_GLOBAL` lie inside it,
  `GIT_CONFIG_NOSYSTEM=1` and `GIT_ALLOW_PROTOCOL=file` (no network). `run-gui.ps1` sets all of that, resets the
  sandbox's app data before each run and the runner only opens repositories inside the sandbox; services and
  update checks are off in QA mode. Never run a qa build outside `run-gui.ps1`, never point a sandbox at real data,
  and keep scenarios, fixtures and screenshots synthetic.
- Tests of the runner: `cargo test -p gitcthulhu --features qa --lib qa::`.
