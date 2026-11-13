# Developing GitCthulhu

GitCthulhu is a Rust workspace (`crates/`); see [`crates/AGENTS.md`](crates/AGENTS.md) for the crate layout and
engineering rules.

## Prerequisites

- Rust, as pinned in `rust-toolchain.toml` (install with rustup).
- Windows: the Windows SDK (provides `fxc.exe`).
- Zig 0.17 on `PATH`: `but-tty` builds libghostty-vt with it. The GUI's default `terminal` feature depends on
  `but-tty`, so `--exclude but-tty` alone does not help. Without Zig use
  `cargo check --workspace --exclude but-tty --exclude gitcthulhu` plus
  `cargo check -p gitcthulhu --no-default-features --features services` for the GUI (see `README.md`); the
  Terminal button is then disabled. `cargo test --workspace` needs the same two exclusions; the `make` targets
  build the whole workspace and so need Zig.
- Linux: `libxcb1-dev libx11-xcb-dev libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libfontconfig-dev
  libfreetype-dev libssl-dev` (GUI and `-sys` crates) and `libdbus-1-dev pkg-config` (keyring);
  `scripts/install-minimal-debian-dependencies.sh` installs the latter on Debian/Ubuntu. On Windows, `pwsh -NoProfile
  -File tools/dev/linux-test.ps1 <cargo arguments>` runs cargo in a Linux container (`--git-min`: Git 2.34.1).
- Optional: `nix develop` (or `direnv allow`) gives a dev shell with Rust and Zig; `cargo-nextest` for faster tests.

## Everyday commands

```sh
make            # fmt, fmt-check, check, clippy
make test       # cargo nextest run --workspace --exclude but-core
cargo test --workspace --exclude but-core
cargo fmt
```

`but-core` still carries GitButler-era tests with known failures (decision D2, slated for the slimming stage), so
the commands above exclude it. CI runs it separately and non-blocking: `cargo test -p but-core` (`make
test-legacy`).

`scripts/cargo-test.sh` deletes generated fixtures before running tests; `scripts/clean.sh` removes build output.

## Debugging

- Logs honour `RUST_LOG` and are written to the log file described below. A release build on Windows has no
  console (`windows_subsystem = "windows"`), so `RUST_LOG` output only reaches the log file there.
- App data, config and cache live under the `gitcthulhu` identity (`crates/gitcthulhu/src/paths.rs`, mirrored by
  `but-path`); `GITCTHULHU_HOME` redirects all of them, which the tests use for throw-away directories.
- `.cargo/config.toml` sets `--cfg tokio_unstable` for Tokio diagnostics.

## Where GitCthulhu keeps its data

| What | Location |
| --- | --- |
| Config: `settings.json`, `state.json`, `repos.json` | `dirs::config_dir()/gitcthulhu` (`%APPDATA%\gitcthulhu`, `~/.config/gitcthulhu`, `~/Library/Application Support/gitcthulhu`) |
| Data (forge storage, ...) | `dirs::data_local_dir()/gitcthulhu` (`%LOCALAPPDATA%\gitcthulhu`, `~/.local/share/gitcthulhu`, `~/Library/Application Support/gitcthulhu`) |
| Logs: `gitcthulhu.log`, rotated at 10 MB, 5 files kept | `<data>/logs` |
| Cache | `dirs::cache_dir()/gitcthulhu` (`%LOCALAPPDATA%\gitcthulhu`, `~/.cache/gitcthulhu`, `~/Library/Caches/gitcthulhu`) |

`GITCTHULHU_HOME` replaces all of them by `config`, `data`, `logs` and `cache` below it. Attach `gitcthulhu.log` to
bug reports.

- **Inside your repositories**: the undo log is kept as Git objects under `refs/gitcthulhu/oplog/main` (a linked
  worktree uses `refs/gitcthulhu/oplog/linked/<id>`). Inspect them with `git for-each-ref refs/gitcthulhu`; remove
  one with `git update-ref -d <ref>` (this drops the undo history of that repository; `git gc` then frees the
  objects).
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

## Installer and updater

`but-installer` (with `scripts/install.sh`) and `but-update` have no built-in endpoints. The installer needs
`GITCTHULHU_RELEASE_URL` (release API base URL) and `GITCTHULHU_RELEASE_PUBLIC_KEY` (minisign key of the releases),
the bootstrap script additionally `GITCTHULHU_INSTALLER_URL`; release information is only accepted from the
configured host, downloads also from its subdomains, always over HTTPS. The macOS installer expects a
`GitCthulhu.app` bundle with `Contents/MacOS/{gitcthulhu,gitcthulhu-askpass,gitcthulhu-editor}`, the Linux one a
`gitcthulhu` binary (plus `gitcthulhu.sig`) next to the platform download. The release pipeline that publishes these
is future work.
