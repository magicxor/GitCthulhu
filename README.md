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

Under construction. The virtual-branch code and the old front ends are gone; the new backend (`gitcthulhu-git`),
the undo log (`gitcthulhu-oplog`) and the GUI (`gitcthulhu`) are being built. There is no usable application yet.

## Building

Requirements:

- Rust, as pinned in [`rust-toolchain.toml`](rust-toolchain.toml)
- Windows: the Windows SDK (`fxc.exe`)
- [Zig 0.17](https://ziglang.org/download/) on `PATH` (builds libghostty-vt for the embedded terminal, `but-tty`;
  the GUI's default `terminal` feature needs it). Without Zig, build the GUI without the terminal:
  `cargo build -p gitcthulhu --no-default-features --features services` (the Terminal button is then disabled).
- Linux: libxcb, xkbcommon-x11, fontconfig and libdbus development packages

```sh
cargo check --workspace --all-targets
cargo test --workspace
```

See [DEVELOPMENT.md](DEVELOPMENT.md) for more.

## License

See [LICENSE.md](LICENSE.md). Code inherited from GitButler remains under its Functional Source License terms.
