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
