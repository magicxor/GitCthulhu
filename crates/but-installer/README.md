# but-installer

A lightweight Rust installer for GitCthulhu on macOS and Linux.

## How It Works

Installation is split into two parts:

1. **Bootstrap script** (`scripts/install.sh`) - Minimal shell script that:
   - Detects OS/architecture
   - Fetches installer metadata from the installer API named by `GITCTHULHU_INSTALLER_URL`
   - Downloads the appropriate installer binary from that host (or a subdomain of it)
   - Executes it

2. **Installer binary** (this crate) - Handles the actual installation:
   - Fetches release information from the release API named by `GITCTHULHU_RELEASE_URL`
   - Downloads the GitCthulhu tarball (macOS) or binary (Linux) and verifies its minisign signature against
     `GITCTHULHU_RELEASE_PUBLIC_KEY`
   - Extracts and installs the app bundle atomically (macOS)
   - Puts the `gitcthulhu` binary (or a symlink into the app bundle) into `~/.local/bin`
   - Offers to add `~/.local/bin` to the shell's `PATH`

There are no built-in endpoints or keys: the installer refuses to run until the release server and the signing
key are configured, and it only accepts release information from the configured host (downloads also from its
subdomains), always over HTTPS.

## Usage

```bash
export GITCTHULHU_RELEASE_URL=https://releases.example.com/releases
export GITCTHULHU_RELEASE_PUBLIC_KEY=<base64 minisign public key>

# Via bootstrap script
GITCTHULHU_INSTALLER_URL=https://releases.example.com/installers sh install.sh
GITCTHULHU_INSTALLER_URL=https://releases.example.com/installers sh install.sh nightly
GITCTHULHU_INSTALLER_URL=https://releases.example.com/installers sh install.sh 0.18.7

# Direct invocation
but-installer                  # Install latest stable
but-installer nightly          # Install nightly
but-installer 0.18.7           # Install specific version (or GITCTHULHU_VERSION=0.18.7)

# From another script: skip the shell-config prompt even in a terminal
GITCTHULHU_NONINTERACTIVE=1 but-installer
```

## Building

```bash
cargo build --release -p but-installer
cargo test -p but-installer
```

The binary is optimized for size (~1.0MB) using system libcurl instead of bundling an HTTP client. Keeping this
installer slim is a priority since it is downloaded before every installation.
