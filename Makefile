# Check all rust code - formatting, cargo check, and clippy.
.PHONY: all
all: fmt fmt-check check clippy

# Perform any automated formatting or clippy fixes that are available.
.PHONY: fix
fix: clippy-fix fmt

# Smaller - sub-checks

# Format: code to match rustfmt configuration.
.PHONY: fmt
fmt:
	cargo fmt

# Lint: code to ensure it matches our rustfmt configuration.
.PHONY: fmt-check
fmt-check:
	cargo fmt --check --all

# Lint: code to ensure there are no compiler errors or warnings.
.PHONY: check
check:
	cargo check --workspace --all-targets

# Lint: code to ensure rust best practices are observed.
.PHONY: clippy
clippy:
	cargo clippy --workspace --all-targets -- -D warnings

# An alias for nextest
.PHONY: test
test: nextest

# Run all maintained tests of the workspace and show all failures in the end. but-core's legacy GitButler-era
# suite is excluded (known failures, decision D2); see test-legacy.
.PHONY: nextest
nextest:
	cargo nextest run --workspace --exclude but-core --no-fail-fast

# but-core's legacy suite: non-blocking, has known failures.
.PHONY: test-legacy
test-legacy:
	cargo nextest run -p but-core --no-fail-fast

# Format: clippy issues --allow-dirty allows the fix to be run even with a dirty
# working tree which can technically cause code to break, but I've never seen it
# happen in practice.
.PHONY: clippy-fix
clippy-fix:
	cargo clippy --workspace --all-targets --fix --allow-dirty
