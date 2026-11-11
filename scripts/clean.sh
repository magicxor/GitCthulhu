#!/bin/sh

echo "Warning: This script deletes the build output and generated test fixtures."
echo "Warning: Run it from the root of your GitCthulhu checkout as ./scripts/clean.sh."
printf "Are you sure you want to delete all the generated artifacts? (y/n) "
read confirmed

if [ "$confirmed" = "${confirmed#[yY]}" ]; then
    echo "Aborting"
    exit 1
fi

echo "Removing rust artifacts..."
rm -rf .rust-analyzer
rm -rf target

echo "Removing rust fixtures..."
rm -rf crates/*/tests/fixtures/generated-do-not-edit
