# Developing GitCthulhu

GitCthulhu is a Rust workspace (`crates/`); see [`crates/AGENTS.md`](crates/AGENTS.md) for the crate layout and
engineering rules.

## Prerequisites

- Rust, as pinned in `rust-toolchain.toml` (install with rustup).
- Windows: the Windows SDK (provides `fxc.exe`).
- Zig 0.17 on `PATH`: `but-tty` builds libghostty-vt with it. Without Zig, add `--exclude but-tty` to cargo
  commands.
- Linux: `libxcb1-dev libx11-xcb-dev libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libfontconfig-dev
  libfreetype-dev libssl-dev` (GUI and `-sys` crates) and `libdbus-1-dev pkg-config` (keyring);
  `scripts/install-minimal-debian-dependencies.sh` installs the latter on Debian/Ubuntu. On Windows, `pwsh -NoProfile
  -File tools/dev/linux-test.ps1 <cargo arguments>` runs cargo in a Linux container (`--git-min`: Git 2.34.1).
- Optional: `nix develop` (or `direnv allow`) gives a dev shell with Rust and Zig; `cargo-nextest` for faster tests.

## Everyday commands

```sh
make            # fmt, fmt-check, check, clippy
make test       # cargo nextest run --workspace
cargo test --workspace
cargo fmt
```

`scripts/cargo-test.sh` deletes generated fixtures before running tests; `scripts/clean.sh` removes build output.

## Debugging

- Logs honour `RUST_LOG`.
- `but-path` decides where app data, config and cache live; tests redirect it to throw-away directories.
- `.cargo/config.toml` sets `--cfg tokio_unstable` for Tokio diagnostics.

## Installer and updater

`but-installer` (with `scripts/install.sh`) and `but-update` are kept as crates but still point at GitButler's
release endpoints (`app.gitbutler.com`, `releases.gitbutler.com`) and the macOS installer expects an app bundle
containing the old desktop binary plus the askpass helper. Re-pointing them at GitCthulhu releases is future work.
