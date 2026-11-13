#!/bin/sh
# GitCthulhu installer bootstrap script
# This script downloads and runs the GitCthulhu installer binary.
#
# There are no built-in endpoints. Configure them through the environment:
#   GITCTHULHU_INSTALLER_URL       base URL of the installer API (https). The installer metadata is
#                                  read from "$GITCTHULHU_INSTALLER_URL/info/<os>/<arch>"; the installer
#                                  binary must be served by the same host or one of its subdomains.
#   GITCTHULHU_RELEASE_URL         base URL of the release API, used by the installer binary (https).
#   GITCTHULHU_RELEASE_PUBLIC_KEY  base64 minisign public key the releases are signed with.
#   GITCTHULHU_NONINTERACTIVE      set to a non-empty value to skip the shell-configuration prompts.

# Wrap everything in main() to protect against partial downloads when
# executed via `curl | sh`. The shell won't execute until the entire
# script has been received.
main() {
  set -eu

  # Check for required commands
  for cmd in curl mktemp grep sed uname chmod tr rm head; do
      if ! command -v "$cmd" >/dev/null 2>&1; then
          echo "Error: Required command '$cmd' not found. Please install it and try again." >&2
          exit 1
      fi
  done

  for var in GITCTHULHU_INSTALLER_URL GITCTHULHU_RELEASE_URL GITCTHULHU_RELEASE_PUBLIC_KEY; do
      eval "value=\${$var:-}"
      if [ -z "$value" ]; then
          echo "Error: $var is not set. This installer has no default release server." >&2
          exit 1
      fi
  done

  # The host of an https URL, or nothing if the URL isn't https, carries user information or has an
  # IP-literal host (whose colons can't be told from a port here).
  https_host() {
      authority=$(printf '%s\n' "$1" | sed -n 's|^https://\([^/?#]*\).*$|\1|p')
      case "$authority" in
          *@*|*\[*) return 0 ;;
      esac
      printf '%s\n' "${authority%%:*}"
  }

  # Succeeds if host $1 is the trusted host $2 or one of its subdomains.
  is_trusted_host() {
      case "$1" in
          "$2"|*".$2") return 0 ;;
          *) return 1 ;;
      esac
  }

  TRUSTED_HOST=$(https_host "$GITCTHULHU_INSTALLER_URL")
  if [ -z "$TRUSTED_HOST" ]; then
      echo "Error: GITCTHULHU_INSTALLER_URL must be an https URL with a host name: $GITCTHULHU_INSTALLER_URL" >&2
      exit 1
  fi
  case "$GITCTHULHU_INSTALLER_URL" in
      *'?'*|*'#'*)
          echo "Error: GITCTHULHU_INSTALLER_URL must not have a query or fragment: $GITCTHULHU_INSTALLER_URL" >&2
          exit 1
          ;;
  esac

  # Detect OS and architecture
  OS=$(uname -s | tr '[:upper:]' '[:lower:]')
  ARCH=$(uname -m)

  # Map OS to installer format
  case "$OS" in
      "darwin") INSTALLER_OS="macos" ;;
      "linux") INSTALLER_OS="linux" ;;
      *)
          echo "Error: This installer currently only supports macOS and Linux. Your OS: $OS" >&2
          exit 1
          ;;
  esac

  # Map architecture to installer format
  case "$ARCH" in
      "x86_64") INSTALLER_ARCH="x86_64" ;;
      "arm64"|"aarch64") INSTALLER_ARCH="aarch64" ;;
      *)
          echo "Error: Unsupported architecture: $ARCH" >&2
          exit 1
          ;;
  esac

  # Create temp files and setup cleanup
  # Use explicit templates for portability across GNU/BSD mktemp variants
  INSTALLER_JSON=$(mktemp "${TMPDIR:-/tmp}/gitcthulhu-installer-json.XXXXXX")
  INSTALLER_BIN=$(mktemp "${TMPDIR:-/tmp}/gitcthulhu-installer-bin.XXXXXX")
  trap 'rm -f "$INSTALLER_JSON" "$INSTALLER_BIN"' EXIT INT TERM

  # Fetch installer metadata and validate effective URL
  INSTALLER_API_URL="${GITCTHULHU_INSTALLER_URL%/}/info/$INSTALLER_OS/$INSTALLER_ARCH"
  echo "Fetching installer information..."

  EFFECTIVE_URL=$(curl --fail --silent --show-error --location --max-redirs 5 --max-time 300 \
      -o "$INSTALLER_JSON" -w '%{url_effective}' "$INSTALLER_API_URL") || {
      echo "Error: Failed to fetch installer information from $INSTALLER_API_URL" >&2
      exit 1
  }

  if [ "$(https_host "$EFFECTIVE_URL")" != "$TRUSTED_HOST" ]; then
      echo "Error: API was redirected to an untrusted URL: $EFFECTIVE_URL" >&2
      exit 1
  fi

  # Parse installer URL from JSON
  INSTALLER_URL=$(grep -o '"url":"[^"]*"' "$INSTALLER_JSON" | head -1 | sed 's/"url":"\(.*\)"/\1/')

  if [ -z "$INSTALLER_URL" ]; then
      echo "Error: Failed to parse installer URL from API response" >&2
      exit 1
  fi

  # Validate URL from API is trusted
  if ! is_trusted_host "$(https_host "$INSTALLER_URL")" "$TRUSTED_HOST"; then
      echo "Error: Installer URL is not from the trusted host $TRUSTED_HOST: $INSTALLER_URL" >&2
      exit 1
  fi

  # Download installer and validate effective URL
  echo "Downloading installer..."

  EFFECTIVE_DOWNLOAD_URL=$(curl --fail --silent --show-error --location --max-redirs 5 --max-time 120 \
      -o "$INSTALLER_BIN" -w '%{url_effective}' "$INSTALLER_URL") || {
      echo "Error: Failed to download installer from $INSTALLER_URL" >&2
      exit 1
  }

  if ! is_trusted_host "$(https_host "$EFFECTIVE_DOWNLOAD_URL")" "$TRUSTED_HOST"; then
      echo "Error: Download was redirected to an untrusted URL: $EFFECTIVE_DOWNLOAD_URL" >&2
      exit 1
  fi

  # Verify downloaded installer
  [ -s "$INSTALLER_BIN" ] || {
      echo "Error: Downloaded installer is empty" >&2
      exit 1
  }

  chmod +x "$INSTALLER_BIN" || {
      echo "Error: Failed to make installer executable" >&2
      exit 1
  }

  # Run installer with forwarded arguments; it reads GITCTHULHU_RELEASE_URL and
  # GITCTHULHU_RELEASE_PUBLIC_KEY from the environment.
  export GITCTHULHU_RELEASE_URL GITCTHULHU_RELEASE_PUBLIC_KEY
  exec "$INSTALLER_BIN" "$@"
}

# Entry point — must be the last line so the shell only executes after the
# entire script has been downloaded.
main "$@"
