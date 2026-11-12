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
- App data, config and cache live under the `gitcthulhu` identity (`crates/gitcthulhu/src/paths.rs`, mirrored by
  `but-path`); `GITCTHULHU_HOME` redirects all of them, which the tests use for throw-away directories.
- `.cargo/config.toml` sets `--cfg tokio_unstable` for Tokio diagnostics.

## Installer and updater

`but-installer` (with `scripts/install.sh`) and `but-update` have no built-in endpoints. The installer needs
`GITCTHULHU_RELEASE_URL` (release API base URL) and `GITCTHULHU_RELEASE_PUBLIC_KEY` (minisign key of the releases),
the bootstrap script additionally `GITCTHULHU_INSTALLER_URL`; release information is only accepted from the
configured host, downloads also from its subdomains, always over HTTPS. The macOS installer expects a
`GitCthulhu.app` bundle with `Contents/MacOS/{gitcthulhu,gitcthulhu-askpass,gitcthulhu-editor}`, the Linux one a
`gitcthulhu` binary (plus `gitcthulhu.sig`) next to the platform download. The release pipeline that publishes these
is future work.
