#!/usr/bin/env sh
# One-line installer & uninstaller for jcode-mcp (jcode-rpc-for-codex)
#
# Install:
#   curl -fsSL https://raw.githubusercontent.com/bridge-for-codex/jcode-rpc-for-codex/main/install.sh | sh
#
# Install specific version or custom directory:
#   curl -fsSL https://raw.githubusercontent.com/bridge-for-codex/jcode-rpc-for-codex/main/install.sh | JCODE_MCP_VERSION=v0.1.0 INSTALL_DIR=/usr/local/bin sh
#
# Uninstall:
#   curl -fsSL https://raw.githubusercontent.com/bridge-for-codex/jcode-rpc-for-codex/main/install.sh | sh -s -- --uninstall

set -eu

REPO="${JCODE_MCP_REPO:-bridge-for-codex/jcode-rpc-for-codex}"
BINARY_NAME="jcode-mcp"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"
VERSION="${JCODE_MCP_VERSION:-latest}"
ACTION="install"

usage() {
  cat <<EOF
Usage: install.sh [OPTIONS]

Options:
  --install              Download and install ${BINARY_NAME} (default)
  --uninstall, --remove  Remove installed ${BINARY_NAME} binary
  --version <TAG>        Install a specific release tag (e.g. v0.1.0)
  --dir <PATH>           Installation directory (default: ~/.local/bin)
  -h, --help             Show this help message

Environment Variables:
  INSTALL_DIR            Installation directory (default: ~/.local/bin)
  JCODE_MCP_VERSION      Release tag to install (default: latest)
EOF
}

while [ $# -gt 0 ]; do
  case "$1" in
    --install)
      ACTION="install"
      shift
      ;;
    --uninstall|--remove|uninstall|remove)
      ACTION="uninstall"
      shift
      ;;
    --version|-v)
      VERSION="$2"
      shift 2
      ;;
    --dir|-d)
      INSTALL_DIR="$2"
      shift 2
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      echo "Error: unknown argument '$1'" >&2
      usage >&2
      exit 1
      ;;
  esac
done

do_uninstall() {
  REMOVED=0
  for candidate in \
    "${INSTALL_DIR}/${BINARY_NAME}" \
    "$HOME/.local/bin/${BINARY_NAME}" \
    "$HOME/.cargo/bin/${BINARY_NAME}" \
    "/usr/local/bin/${BINARY_NAME}"
  do
    if [ -f "$candidate" ]; then
      if [ -w "$(dirname "$candidate")" ]; then
        rm -f "$candidate"
      else
        sudo rm -f "$candidate"
      fi
      echo "Removed: $candidate"
      REMOVED=1
    fi
  done

  if [ "$REMOVED" -eq 0 ]; then
    echo "${BINARY_NAME} was not found in ${INSTALL_DIR}, ~/.local/bin, ~/.cargo/bin, or /usr/local/bin."
  else
    echo "${BINARY_NAME} has been uninstalled."
  fi
}

if [ "$ACTION" = "uninstall" ]; then
  do_uninstall
  exit 0
fi

detect_target() {
  OS="$(uname -s)"
  ARCH="$(uname -m)"

  case "$OS" in
    Darwin)
      case "$ARCH" in
        arm64|aarch64) echo "aarch64-apple-darwin" ;;
        x86_64|amd64)  echo "x86_64-apple-darwin" ;;
        *)
          echo "Error: unsupported macOS architecture: $ARCH" >&2
          exit 1
          ;;
      esac
      ;;
    Linux)
      case "$ARCH" in
        x86_64|amd64)  echo "x86_64-unknown-linux-gnu" ;;
        arm64|aarch64) echo "aarch64-unknown-linux-gnu" ;;
        *)
          echo "Error: unsupported Linux architecture: $ARCH" >&2
          exit 1
          ;;
      esac
      ;;
    *)
      echo "Error: unsupported operating system: $OS" >&2
      exit 1
      ;;
  esac
}

resolve_version() {
  if [ "$VERSION" != "latest" ]; then
    echo "$VERSION"
    return
  fi

  API_URL="https://api.github.com/repos/${REPO}/releases/latest"
  TAG="$(curl -fsSL "$API_URL" | sed -n 's/.*"tag_name":[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1)"
  if [ -z "$TAG" ]; then
    echo "Error: failed to resolve latest release tag from ${API_URL}" >&2
    exit 1
  fi
  echo "$TAG"
}

verify_sha256() {
  ARCHIVE_PATH="$1"
  CHECKSUM_PATH="$2"

  EXPECTED="$(awk '{print $1}' "$CHECKSUM_PATH")"
  if command -v sha256sum >/dev/null 2>&1; then
    ACTUAL="$(sha256sum "$ARCHIVE_PATH" | awk '{print $1}')"
  elif command -v shasum >/dev/null 2>&1; then
    ACTUAL="$(shasum -a 256 "$ARCHIVE_PATH" | awk '{print $1}')"
  else
    echo "Warning: neither sha256sum nor shasum found; skipping checksum verification." >&2
    return 0
  fi

  if [ "$EXPECTED" != "$ACTUAL" ]; then
    echo "Error: SHA256 checksum mismatch!" >&2
    echo "  Expected: $EXPECTED" >&2
    echo "  Actual:   $ACTUAL" >&2
    exit 1
  fi
}

TARGET="$(detect_target)"
TAG="$(resolve_version)"
ARCHIVE_NAME="${BINARY_NAME}-${TAG}-${TARGET}.tar.gz"
BASE_URL="https://github.com/${REPO}/releases/download/${TAG}"

TMP_DIR="$(mktemp -d 2>/dev/null || mktemp -d -t jcode-mcp-install)"
cleanup() {
  rm -rf "$TMP_DIR"
}
trap cleanup EXIT INT TERM

echo "Downloading ${BINARY_NAME} ${TAG} for ${TARGET}..."
curl -fsSL "${BASE_URL}/${ARCHIVE_NAME}" -o "${TMP_DIR}/${ARCHIVE_NAME}"
curl -fsSL "${BASE_URL}/${ARCHIVE_NAME}.sha256" -o "${TMP_DIR}/${ARCHIVE_NAME}.sha256"

echo "Verifying SHA256 checksum..."
verify_sha256 "${TMP_DIR}/${ARCHIVE_NAME}" "${TMP_DIR}/${ARCHIVE_NAME}.sha256"

tar -xzf "${TMP_DIR}/${ARCHIVE_NAME}" -C "${TMP_DIR}"
EXTRACTED_BIN="${TMP_DIR}/${BINARY_NAME}-${TAG}-${TARGET}/${BINARY_NAME}"

mkdir -p "$INSTALL_DIR"
if [ -w "$INSTALL_DIR" ]; then
  install -m 755 "$EXTRACTED_BIN" "${INSTALL_DIR}/${BINARY_NAME}"
else
  sudo install -m 755 "$EXTRACTED_BIN" "${INSTALL_DIR}/${BINARY_NAME}"
fi

echo "Installed ${BINARY_NAME} ${TAG} to ${INSTALL_DIR}/${BINARY_NAME}"
case ":${PATH}:" in
  *":${INSTALL_DIR}:"*) ;;
  *)
    echo ""
    echo "Note: ${INSTALL_DIR} is not in your PATH. Add it to your shell profile:"
    echo "  export PATH=\"${INSTALL_DIR}:\$PATH\""
    ;;
esac
