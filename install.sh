#!/bin/sh
# lumina installer — fetches a prebuilt `lmn` binary from GitHub Releases.
#
#   curl -fsSL https://raw.githubusercontent.com/gmfc/lumina/main/install.sh | sh
#
# Environment overrides:
#   LMN_VERSION      release tag to install (default: latest), e.g. v0.1.0
#   LMN_INSTALL_DIR  where to put the binary (default: $HOME/.local/bin)
#   LMN_REPO         owner/repo to download from (default: gmfc/lumina)
#
# POSIX sh — works under sh / dash / bash / zsh on macOS and Linux.
set -eu

REPO="${LMN_REPO:-gmfc/lumina}"
VERSION="${LMN_VERSION:-latest}"
INSTALL_DIR="${LMN_INSTALL_DIR:-$HOME/.local/bin}"
BIN="lmn"

say()  { printf '%s\n' "$*"; }
warn() { printf '%s\n' "warning: $*" >&2; }
err()  { printf '%s\n' "error: $*" >&2; exit 1; }

# Pick a downloader up front.
if command -v curl >/dev/null 2>&1; then
  dl() { curl -fSL --proto '=https' --tlsv1.2 "$1" -o "$2"; }
elif command -v wget >/dev/null 2>&1; then
  dl() { wget -q "$1" -O "$2"; }
else
  err "need curl or wget on PATH"
fi
command -v tar >/dev/null 2>&1 || err "need tar on PATH"

os="$(uname -s)"
arch="$(uname -m)"

case "$os" in
  Linux)  os_part="unknown-linux-gnu" ;;
  Darwin) os_part="apple-darwin" ;;
  *) err "unsupported OS '$os' — on Windows use install.ps1 instead" ;;
esac

case "$arch" in
  x86_64 | amd64)        arch_part="x86_64" ;;
  aarch64 | arm64)       arch_part="aarch64" ;;
  *) err "unsupported architecture '$arch'" ;;
esac

target="${arch_part}-${os_part}"
asset="lmn-${target}.tar.gz"

if [ "$VERSION" = "latest" ]; then
  base="https://github.com/${REPO}/releases/latest/download"
else
  base="https://github.com/${REPO}/releases/download/${VERSION}"
fi
url="${base}/${asset}"

tmp="$(mktemp -d 2>/dev/null || mktemp -d -t lmn)"
trap 'rm -rf "$tmp"' EXIT INT TERM

say "downloading ${asset} (${VERSION}) ..."
dl "$url" "$tmp/$asset" || err "download failed: $url"

# Verify the SHA-256 checksum when the sidecar file is published and a hashing tool exists.
if dl "${url}.sha256" "$tmp/$asset.sha256" 2>/dev/null; then
  expected="$(awk '{print $1}' "$tmp/$asset.sha256")"
  if command -v sha256sum >/dev/null 2>&1; then
    actual="$(sha256sum "$tmp/$asset" | awk '{print $1}')"
  elif command -v shasum >/dev/null 2>&1; then
    actual="$(shasum -a 256 "$tmp/$asset" | awk '{print $1}')"
  else
    actual=""
    warn "no sha256sum/shasum found — skipping checksum verification"
  fi
  if [ -n "$actual" ] && [ "$expected" != "$actual" ]; then
    err "checksum mismatch (expected $expected, got $actual)"
  fi
else
  warn "no published checksum for ${asset} — skipping verification"
fi

tar -xzf "$tmp/$asset" -C "$tmp"

# The archive holds a single versioned folder (lmn-<version>-<target>/lmn); locate the
# binary rather than assume the folder name, so `latest` works without knowing the version.
src="$(find "$tmp" -type f -name "$BIN" | head -n1)"
[ -n "$src" ] || err "could not find '$BIN' inside the downloaded archive"

mkdir -p "$INSTALL_DIR"
# Atomic install: stage in the target dir, then rename over any existing binary. A rename
# within one filesystem is atomic and leaves an already-running lmn on its old inode, so
# re-running this script (i.e. `lmn update`) upgrades cleanly even while the editor is open.
staged="$INSTALL_DIR/.$BIN.new.$$"
cp "$src" "$staged"
chmod 0755 "$staged"
mv -f "$staged" "$INSTALL_DIR/$BIN"
say "installed $BIN -> $INSTALL_DIR/$BIN"

# Nudge the user if the install dir isn't on PATH yet.
case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *)
    say ""
    say "note: $INSTALL_DIR is not on your PATH. Add it, then restart your shell:"
    say "  echo 'export PATH=\"$INSTALL_DIR:\$PATH\"' >> ~/.profile"
    say "  # bash: ~/.bashrc   zsh: ~/.zshrc   fish: set -U fish_user_paths $INSTALL_DIR"
    ;;
esac

say ""
say "done. Open the current directory with:"
say "  $BIN ."
