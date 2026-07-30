#!/bin/sh
# Install Gabriel from a released archive.
#
# Deliberately POSIX sh with no dependencies beyond curl-or-wget and tar: the
# whole point is that someone can run it on a machine that has nothing set up.
#
#   curl -fsSL <base>/install.sh | sh
#   curl -fsSL <base>/install.sh | GABRIEL_VERSION=v0.1.0 sh
#   curl -fsSL <base>/install.sh | GABRIEL_INSTALL_DIR=/usr/local/bin sh
#
# The checksum is verified before anything is copied. If it cannot be verified,
# the install stops rather than continuing hopefully — a download that arrived
# wrong is the one case where doing nothing is clearly right.

set -eu

BASE_URL="${GABRIEL_BASE_URL:-https://github.com/Alban-M/Gabrielbrowser/releases/download}"
VERSION="${GABRIEL_VERSION:-latest}"
INSTALL_DIR="${GABRIEL_INSTALL_DIR:-$HOME/.local/bin}"

say() { printf '%s\n' "$*"; }
die() { printf 'install: %s\n' "$*" >&2; exit 1; }

need() {
    command -v "$1" >/dev/null 2>&1 || die "$1 is required but not installed"
}

# ── work out which archive this machine needs ────────────────────────────────

detect_target() {
    _os=$(uname -s)
    _arch=$(uname -m)

    case "$_os" in
        Darwin) _os_part="apple-darwin" ;;
        Linux)  _os_part="unknown-linux-gnu" ;;
        MINGW*|MSYS*|CYGWIN*)
            die "on Windows, download the .zip from $BASE_URL and add gabriel.exe to your PATH"
            ;;
        *) die "unsupported operating system: $_os" ;;
    esac

    case "$_arch" in
        x86_64|amd64)  _arch_part="x86_64" ;;
        arm64|aarch64) _arch_part="aarch64" ;;
        *) die "unsupported architecture: $_arch" ;;
    esac

    printf '%s-%s' "$_arch_part" "$_os_part"
}

# Note on style: POSIX sh has no `local`, so anything assigned in a function is
# global. Every function variable here is prefixed to keep it from clobbering a
# caller's — which it silently did, once.
download() {
    _url="$1"
    _out="$2"
    if command -v curl >/dev/null 2>&1; then
        # --fail so an HTML error page is never mistaken for an archive.
        curl -fsSL "$_url" -o "$_out" || return 1
    elif command -v wget >/dev/null 2>&1; then
        wget -qO "$_out" "$_url" || return 1
    else
        die "neither curl nor wget is available"
    fi
}

verify() {
    _archive="$1"
    _sums="$2"

    _expected=$(grep -Eo '[0-9a-fA-F]{64}' "$_sums" | head -1 | tr 'A-Z' 'a-z')
    [ -n "$_expected" ] || die "the checksum file contains no digest"

    if command -v shasum >/dev/null 2>&1; then
        _actual=$(shasum -a 256 "$_archive" | cut -d' ' -f1)
    elif command -v sha256sum >/dev/null 2>&1; then
        _actual=$(sha256sum "$_archive" | cut -d' ' -f1)
    else
        die "no sha256 tool found; refusing to install an unverified download"
    fi

    [ "$_expected" = "$_actual" ] || die "checksum mismatch
  expected $_expected
  actual   $_actual
The download is corrupt or has been tampered with. Nothing was installed."
}

copy_binary() {
    _from="$1"
    _to="$2"
    # `install` is not on every minimal image, so fall back to cp.
    install -m 755 "$_from" "$_to" 2>/dev/null && return 0
    cp "$_from" "$_to" 2>/dev/null || return 1
    chmod 755 "$_to" 2>/dev/null || return 1
    return 0
}

# ── run ──────────────────────────────────────────────────────────────────────

need tar
need uname

target=$(detect_target)
say "Gabriel installer"
say "  platform : $target"
say "  version  : $VERSION"

# A temporary directory that is cleaned up on any exit, including a failure.
tmp=$(mktemp -d 2>/dev/null || mktemp -d -t gabriel)
trap 'rm -rf "$tmp"' EXIT INT TERM

if [ "$VERSION" = "latest" ]; then
    # `latest/download` is a redirect GitHub maintains; other hosts may not, so
    # a specific version can always be given instead.
    url_base="${BASE_URL%/download}/latest/download"
    name="gabriel-latest-$target"
else
    url_base="$BASE_URL/$VERSION"
    name="gabriel-$VERSION-$target"
fi

archive="$name.tar.gz"
say "  source   : $url_base/$archive"
say ""

download "$url_base/$archive" "$tmp/$archive" \
    || die "could not download $archive
Check that a release exists for this platform at:
  $url_base"

download "$url_base/$archive.sha256" "$tmp/$archive.sha256" \
    || die "could not download the checksum for $archive; refusing to install unverified"

verify "$tmp/$archive" "$tmp/$archive.sha256"
say "checksum verified"

tar xzf "$tmp/$archive" -C "$tmp"
binary=$(find "$tmp" -name gabriel -type f | head -1)
[ -n "$binary" ] || die "no gabriel binary inside the archive"

mkdir -p "$INSTALL_DIR" 2>/dev/null || die "cannot create $INSTALL_DIR
Choose somewhere else with GABRIEL_INSTALL_DIR."

# `set -e` is unreliable for the last command of an `||` list — some shells
# exit, some carry on — so the failure is checked explicitly. Without this the
# installer reported success after failing to write, which is the worst possible
# outcome for something people run piped into a shell.
if ! copy_binary "$binary" "$INSTALL_DIR/gabriel"; then
    die "cannot write to $INSTALL_DIR
Choose a writable directory with GABRIEL_INSTALL_DIR, for example:
  curl -fsSL <url>/install.sh | GABRIEL_INSTALL_DIR=\$HOME/bin sh"
fi

say "installed to $INSTALL_DIR/gabriel"

# macOS quarantines anything downloaded, and an unsigned binary is refused with
# a dialog that does not explain itself. Clearing it here is the difference
# between "it works" and "macOS says it is damaged".
if [ "$(uname -s)" = "Darwin" ] && command -v xattr >/dev/null 2>&1; then
    xattr -d com.apple.quarantine "$INSTALL_DIR/gabriel" 2>/dev/null || true
fi

say ""
case ":$PATH:" in
    *":$INSTALL_DIR:"*)
        say "Run: gabriel doctor"
        ;;
    *)
        say "$INSTALL_DIR is not on your PATH. Add it:"
        say "  export PATH=\"\$PATH:$INSTALL_DIR\""
        say ""
        say "Then run: gabriel doctor"
        ;;
esac
