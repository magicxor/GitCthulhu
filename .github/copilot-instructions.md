# GitCthulhu - Copilot instructions

GitCthulhu is a plain Git GUI client written in Rust (forked from GitButler, with virtual branches
removed). Follow the nearest applicable `AGENTS.md`; `AGENTS.md` and `crates/AGENTS.md` hold the
project rules and the intended crate layout.

- Build: `cargo check --workspace --all-targets` (Windows SDK needed on Windows; Zig 0.17 for `but-tty`).
- Test: `cargo test --workspace --exclude but-core`; format with `cargo fmt`; lint with `cargo clippy --workspace --all-targets`.
- Read Git data with `gix`; mutate through the `git` executable. No `git2` in new code.
- Use `but-testsupport` for fixtures and temporary repositories, not ad-hoc temp dirs.

## Review guidelines

- Do not report type errors or formatting; CI catches those.
