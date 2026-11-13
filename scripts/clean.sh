#!/bin/sh
# Deletes the generated test fixtures. With --target it also deletes the build output (target/ and
# .rust-analyzer/). Do not use --target in a main checkout whose target/ seeds agent worktrees
# (AGENTS.md, tools/dev/seed-target.ps1): every worktree would then rebuild all dependencies.

delete_target=no
if [ "${1:-}" = "--target" ]; then
    delete_target=yes
elif [ -n "${1:-}" ]; then
    echo "Usage: ./scripts/clean.sh [--target]"
    exit 2
fi

echo "Run this from the root of your GitCthulhu checkout as ./scripts/clean.sh."
if [ "$delete_target" = yes ]; then
    echo "Warning: this deletes the generated test fixtures AND the build output (target/)."
else
    echo "This deletes the generated test fixtures (pass --target to also delete target/)."
fi
printf "Continue? (y/n) "
read confirmed

if [ "$confirmed" = "${confirmed#[yY]}" ]; then
    echo "Aborting"
    exit 1
fi

if [ "$delete_target" = yes ]; then
    echo "Removing rust artifacts..."
    rm -rf .rust-analyzer
    rm -rf target
fi

echo "Removing rust fixtures..."
rm -rf crates/*/tests/fixtures/generated-do-not-edit
