#!/bin/sh
set -eu

REPO="KSDaemon/seshat"
INSTALL_DIR="${HOME}/.local/bin"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

dry_run=false

usage() {
  cat <<EOF
Usage: $0 [--dry-run]

Options:
  --dry-run  Show planned actions without downloading or installing
  --help     Show this help message
EOF
  exit 0
}

for arg in "$@"; do
  case "$arg" in
    --dry-run)
      dry_run=true
      ;;
    --help|-h)
      usage
      ;;
    *)
      echo "Unknown argument: $arg" >&2
      usage
      ;;
  esac
done

detect_target() {
  os=$(uname -s)
  arch=$(uname -m)

  case "$arch" in
    x86_64|amd64) arch="x86_64" ;;
    aarch64|arm64) arch="aarch64" ;;
    armv7l) arch="armv7" ;;
    *)
      echo "${RED}Error: Unsupported architecture: $arch${NC}" >&2
      echo "See available releases: https://github.com/${REPO}/releases" >&2
      exit 1
      ;;
  esac

  case "$os" in
    Linux)
      if grep -qi microsoft /proc/version 2>/dev/null; then
        echo "${YELLOW}Notice: WSL detected. Using Linux binary.${NC}" >&2
      fi
      target="${arch}-unknown-linux-gnu"
      ;;
    Darwin)
      target="${arch}-apple-darwin"
      ;;
    *)
      echo "${RED}Error: Unsupported OS: $os${NC}" >&2
      echo "See available releases: https://github.com/${REPO}/releases" >&2
      exit 1
      ;;
  esac

  echo "$target"
}

target=$(detect_target)
archive="seshat-${target}.tar.gz"
release_url="https://github.com/${REPO}/releases"

echo "Detected target: ${target}"

if $dry_run; then
  echo "Would fetch latest release from: ${release_url}"
  echo "Would download: ${archive}"
  echo "Would download: sha256sums.txt"
  echo "Would verify SHA256 checksum"
  echo "Would extract ${archive}"
  echo "Would find seshat binary inside"
  echo "Would install seshat to ${INSTALL_DIR}/seshat"
  echo "Would check if ${INSTALL_DIR} is in PATH"
  exit 0
fi

echo "Fetching latest release..."
api_url="https://api.github.com/repos/${REPO}/releases/latest"
if command -v curl >/dev/null 2>&1; then
  tag=$(curl -sSL "${api_url}" | grep '"tag_name"' | head -1 | sed 's/.*"tag_name": *"\(.*\)".*/\1/')
elif command -v wget >/dev/null 2>&1; then
  tag=$(wget -qO- "${api_url}" | grep '"tag_name"' | head -1 | sed 's/.*"tag_name": *"\(.*\)".*/\1/')
else
  echo "${RED}Error: Neither curl nor wget found. Install one to continue.${NC}" >&2
  exit 1
fi

if [ -z "$tag" ]; then
  echo "${RED}Error: Could not determine latest release tag.${NC}" >&2
  echo "Check: ${release_url}" >&2
  exit 1
fi

echo "Latest release: ${tag}"

download_url="https://github.com/${REPO}/releases/download/${tag}"
tmpdir=$(mktemp -d)
trap 'rm -rf "$tmpdir"' EXIT

download() {
  url="$1"
  dest="$2"
  if command -v curl >/dev/null 2>&1; then
    curl -sSL -o "$dest" "$url"
  else
    wget -q -O "$dest" "$url"
  fi
}

echo "Downloading ${archive}..."
download "${download_url}/${archive}" "${tmpdir}/${archive}"

echo "Downloading sha256sums.txt..."
download "${download_url}/sha256sums.txt" "${tmpdir}/sha256sums.txt"

echo "Verifying checksum..."
if command -v sha256sum >/dev/null 2>&1; then
  SELF_CHECKSUM=$(cd "$tmpdir" && sha256sum "$archive" | awk '{print $1}')
  EXPECTED_CHECKSUM=$(grep "$archive" "$tmpdir/sha256sums.txt" | awk '{print $1}')
elif command -v shasum >/dev/null 2>&1; then
  SELF_CHECKSUM=$(cd "$tmpdir" && shasum -a 256 "$archive" | awk '{print $1}')
  EXPECTED_CHECKSUM=$(grep "$archive" "$tmpdir/sha256sums.txt" | awk '{print $1}')
else
  echo "${RED}Error: No sha256sum or shasum command found.${NC}" >&2
  exit 1
fi

if [ -z "$EXPECTED_CHECKSUM" ]; then
  echo "${RED}Error: Could not find checksum for ${archive} in sha256sums.txt${NC}" >&2
  exit 1
fi

if [ "$SELF_CHECKSUM" != "$EXPECTED_CHECKSUM" ]; then
  echo "${RED}Error: Checksum verification failed!${NC}" >&2
  echo "Expected: ${EXPECTED_CHECKSUM}" >&2
  echo "Got:      ${SELF_CHECKSUM}" >&2
  exit 1
fi

echo "${GREEN}Checksum verified.${NC}"

echo "Extracting ${archive}..."
tar xzf "${tmpdir}/${archive}" -C "$tmpdir"

binary_path=$(find "$tmpdir" -name seshat -type f | head -1)
if [ -z "$binary_path" ]; then
  echo "${RED}Error: Could not find seshat binary in extracted archive.${NC}" >&2
  exit 1
fi

echo "${GREEN}Successfully extracted seshat binary.${NC}"

echo "Installing seshat to ${INSTALL_DIR}..."
mkdir -p "$INSTALL_DIR"
cp "$binary_path" "${INSTALL_DIR}/seshat"
chmod +x "${INSTALL_DIR}/seshat"

case ":${PATH}:" in
  *:"${INSTALL_DIR}":*)
    ;;
  *)
    echo "${YELLOW}Warning: ${INSTALL_DIR} is not in your PATH.${NC}" >&2
    echo "  Add this to your shell profile (~/.profile, ~/.bashrc, or ~/.zshrc):" >&2
    echo "  export PATH=\"${INSTALL_DIR}:\$PATH\"" >&2
    ;;
esac

echo "${GREEN}Seshat ${tag} installed successfully to ${INSTALL_DIR}/seshat${NC}"
