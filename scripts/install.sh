#!/usr/bin/env bash
# Install raven-tui from the latest GitHub release into ~/.raven-hotel/bin/
#
# One-liner:
#   curl -fsSL https://raw.githubusercontent.com/ixmon/raven-harness/main/tui/scripts/install.sh | bash
#
# Opt-out of PATH setup (interactive prompt or instructions):
#   RAVEN_INSTALL_NO_PATH=1 curl -fsSL ... | bash

set -euo pipefail

REPO="ixmon/raven-harness"
BINARY_NAME="raven-tui"
INSTALL_DIR="${RAVEN_INSTALL_DIR:-$HOME/.raven-hotel/bin}"
DATA_DIR="${RAVEN_INSTALL_DATA_DIR:-$HOME/.raven-hotel}"
RELEASE_API="https://api.github.com/repos/${REPO}/releases/latest"
DOWNLOAD_BASE="https://github.com/${REPO}/releases/latest/download"
INSTALL_TMP=""

cleanup() {
  [[ -n "$INSTALL_TMP" && -d "$INSTALL_TMP" ]] && rm -rf "$INSTALL_TMP"
}
trap cleanup EXIT

info()  { printf '==> %s\n' "$*"; }
warn()  { printf 'warning: %s\n' "$*" >&2; }
die()   { printf 'error: %s\n' "$*" >&2; exit 1; }

is_interactive() {
  [[ -t 0 && -t 1 ]]
}

detect_target() {
  local os arch
  os="$(uname -s)"
  arch="$(uname -m)"

  case "${os}-${arch}" in
    Linux-x86_64|Linux-amd64)
      echo "x86_64-unknown-linux-gnu"
      ;;
    Linux-aarch64|Linux-arm64)
      echo "aarch64-unknown-linux-gnu"
      ;;
    Darwin-x86_64)
      echo "x86_64-apple-darwin"
      ;;
    Darwin-arm64)
      echo "aarch64-apple-darwin"
      ;;
    *)
      die "unsupported platform: ${os} ${arch}"
      ;;
  esac
}

fetch_latest_version() {
  if command -v jq >/dev/null 2>&1; then
    curl -fsSL "$RELEASE_API" | jq -r '.tag_name'
    return
  fi

  curl -fsSL "$RELEASE_API" \
    | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' \
    | head -n1
}

path_line_for_shell() {
  local shell_path="$1"
  case "$(basename "$shell_path")" in
    fish)
      printf 'fish_add_path -g %q\n' "$INSTALL_DIR"
      ;;
    *)
      printf 'export PATH=%q:$PATH\n' "$INSTALL_DIR"
      ;;
  esac
}

shell_rc_file() {
  local shell_path="$1"
  case "$(basename "$shell_path")" in
    bash)
      if [[ "$(uname -s)" == "Darwin" && -f "$HOME/.bash_profile" ]]; then
        echo "$HOME/.bash_profile"
      else
        echo "$HOME/.bashrc"
      fi
      ;;
    zsh)
      echo "$HOME/.zshrc"
      ;;
    fish)
      echo "$HOME/.config/fish/config.fish"
      ;;
    *)
      echo "$HOME/.profile"
      ;;
  esac
}

path_already_configured() {
  local rc="$1"
  if [[ ":$PATH:" == *":${INSTALL_DIR}:"* ]]; then
    return 0
  fi
  if [[ -f "$rc" ]] && grep -q 'raven-hotel/bin' "$rc" 2>/dev/null; then
    return 0
  fi
  return 1
}

print_path_instructions() {
  local shell_path="${SHELL:-/bin/bash}"
  local line
  line="$(path_line_for_shell "$shell_path")"
  local rc
  rc="$(shell_rc_file "$shell_path")"

  cat <<EOF

Add Raven to your PATH by appending this line to ${rc}:

  ${line}

Then reload your shell (e.g. source ${rc}) or open a new terminal.

Run: ${INSTALL_DIR}/${BINARY_NAME}
EOF
}

maybe_configure_path() {
  if [[ "${RAVEN_INSTALL_NO_PATH:-}" == "1" ]]; then
    info "Skipping PATH setup (RAVEN_INSTALL_NO_PATH=1)"
    print_path_instructions
    return
  fi

  local shell_path="${SHELL:-/bin/bash}"
  local rc
  rc="$(shell_rc_file "$shell_path")"
  local line
  line="$(path_line_for_shell "$shell_path")"

  if path_already_configured "$rc"; then
    info "PATH already includes ${INSTALL_DIR} (or shell rc is configured)"
    return
  fi

  if ! is_interactive; then
    info "Non-interactive install — not modifying shell rc"
    print_path_instructions
    return
  fi

  printf '\nAdd %s to your PATH via %s?\n  %s\n' "$INSTALL_DIR" "$rc" "$line"
  printf '[y/N] '
  local answer=""
  read -r answer || answer=""
  case "$answer" in
    y|Y|yes|Yes|YES)
      mkdir -p "$(dirname "$rc")"
      {
        printf '\n# Added by raven-hotel install script\n'
        printf '%s\n' "$line"
      } >>"$rc"
      info "Appended PATH line to ${rc}"
      info "Reload with: source ${rc}"
      ;;
    *)
      info "Skipped shell rc modification"
      print_path_instructions
      ;;
  esac
}

write_install_metadata() {
  local version="$1"
  local target="$2"
  mkdir -p "$DATA_DIR"
  cat >"${DATA_DIR}/install.json" <<EOF
{
  "version": "${version}",
  "target": "${target}",
  "binary": "${INSTALL_DIR}/${BINARY_NAME}",
  "installed_at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
}
EOF
}

main() {
  local target version asset url extracted

  target="$(detect_target)"
  version="$(fetch_latest_version)"
  [[ -n "$version" ]] || die "could not determine latest release version"

  asset="${BINARY_NAME}-${target}.tar.gz"
  url="${DOWNLOAD_BASE}/${asset}"

  info "Installing ${BINARY_NAME} ${version} for ${target}"
  info "Destination: ${INSTALL_DIR}/${BINARY_NAME}"

  mkdir -p "$INSTALL_DIR"
  INSTALL_TMP="$(mktemp -d)"

  info "Downloading ${url}"
  curl -fsSL "$url" -o "${INSTALL_TMP}/${asset}"

  tar xzf "${INSTALL_TMP}/${asset}" -C "$INSTALL_TMP"
  extracted="$(find "$INSTALL_TMP" -name "$BINARY_NAME" -type f ! -path '*/.*' | head -n1)"
  [[ -n "$extracted" && -f "$extracted" ]] || die "could not find ${BINARY_NAME} in archive"

  install -m 0755 "$extracted" "${INSTALL_DIR}/${BINARY_NAME}"
  write_install_metadata "$version" "$target"

  info "Installed ${INSTALL_DIR}/${BINARY_NAME}"
  maybe_configure_path

  if [[ ":$PATH:" == *":${INSTALL_DIR}:"* ]]; then
    info "Ready to run: ${BINARY_NAME}"
  else
    info "Run once PATH is set: ${INSTALL_DIR}/${BINARY_NAME}"
  fi
}

main "$@"