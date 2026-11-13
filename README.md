# GitCthulhu

GitCthulhu is a deliberately boring, plain Git GUI client. It is forked from
[GitButler](https://github.com/gitbutlerapp/gitbutler) with virtual branches, stacks and the web/Tauri front end
removed: it works with ordinary Git branches, the index and the working tree, like any standard Git client.

- **GUI**: Rust, built on [gpui-kit](https://crates.io/crates/gpui-kit), with a layout modelled on Gitnuro. The
  long-term feature direction is GitKraken.
- **Kept from GitButler**: forge integrations (GitHub, GitLab, Bitbucket), AI assistance (for example commit
  message generation), snapshot-based undo/redo, an embedded terminal, and the installer/updater crates.
- **Git engine**: reads through `gix` in-process, mutations through the `git` executable so hooks, signing and
  credential helpers behave as usual.

## Status

Milestones 1 and 2 are implemented: the repository view (commit graph, branches, tags, stashes, remotes), staging
and committing, diffs, blame and file history, interactive rebase, undo/redo, the embedded terminal, AI commit
messages, forge accounts with pull-request badges, worktrees and an update notice.

## Building

Requirements:

- Rust, as pinned in [`rust-toolchain.toml`](rust-toolchain.toml)
- Windows: the Windows SDK (`fxc.exe`)
- [Zig 0.17](https://ziglang.org/download/) on `PATH` (builds libghostty-vt for the embedded terminal, `but-tty`;
  the GUI's default `terminal` feature needs it). Without Zig, build the GUI without the terminal:
  `cargo build -p gitcthulhu --no-default-features --features services` (the Terminal button is then disabled).
  `--exclude but-tty` alone is not enough: the GUI's `terminal` feature pulls `but-tty` back in, so workspace-wide
  commands also need `--exclude gitcthulhu`.
- Linux: the development packages listed in [DEVELOPMENT.md](DEVELOPMENT.md) (libxcb, libx11-xcb, xkbcommon,
  xkbcommon-x11, wayland, fontconfig, freetype, openssl and libdbus)
- At runtime: a `git` executable on `PATH`, version 2.34.1 or newer

```sh
cargo check --workspace --all-targets
cargo test --workspace --exclude but-core
```

`but-core` still carries GitButler-era tests with known failures (decision D2); CI runs them non-blocking
(`cargo test -p but-core`).

## Running

```sh
cargo run -p gitcthulhu
```

or `cargo build --release -p gitcthulhu`: the binaries are in `target/release`. `gitcthulhu` (the GUI),
`gitcthulhu-askpass` (credential helper) and `gitcthulhu-editor` (editor helper) must stay in the same directory.
A redistributed package should also carry the font licences, `crates/gitcthulhu/assets/fonts/LICENSE-*.txt` (the
About dialog shows them too).

See [DEVELOPMENT.md](DEVELOPMENT.md) for more, including where the application keeps its data.

## License

See [LICENSE.md](LICENSE.md). Code inherited from GitButler remains under its Functional Source License terms.
