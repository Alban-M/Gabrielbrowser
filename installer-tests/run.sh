#!/usr/bin/env bash
# Regression suite for install.sh.
#
# An installer is a trust boundary: it is the one piece of software a user runs
# before they have any reason to trust it, usually piped straight from a URL
# into a shell. It has to refuse clearly when anything is off, and it has to be
# provably safe to re-run.
#
# Reading the script proves none of that — the first real bug here was a POSIX
# scoping mistake invisible on the page and obvious on the first execution. So
# every scenario below runs the real installer against a real HTTP server
# serving a real archive.
#
#   ./installer-tests/run.sh [path-to-gabriel-binary]

set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
INSTALLER="$ROOT/install.sh"
BINARY="${1:-$ROOT/target/release/gabriel}"
PORT="${INSTALLER_TEST_PORT:-8791}"
WORK="$(mktemp -d)"
PASSED=0
FAILED=0

cleanup() {
    [ -n "${SERVER_PID:-}" ] && kill "$SERVER_PID" 2>/dev/null
    chmod -R u+w "$WORK" 2>/dev/null
    rm -rf "$WORK"
}
trap cleanup EXIT INT TERM

note() { printf '  %s\n' "$*"; }

check() {
    local name="$1" condition="$2"
    if eval "$condition"; then
        printf '  ✓ %s\n' "$name"
        PASSED=$((PASSED + 1))
    else
        printf '  ✗ %s\n' "$name"
        printf '    expected: %s\n' "$condition"
        FAILED=$((FAILED + 1))
    fi
}

scenario() { printf '\n%s\n' "$1"; }

# ── fixtures ────────────────────────────────────────────────────────────────

[ -x "$BINARY" ] || {
    echo "no binary at $BINARY — run: cargo build --release" >&2
    exit 2
}

case "$(uname -s)" in
    Darwin) TARGET="$(uname -m | sed 's/arm64/aarch64/')-apple-darwin" ;;
    Linux)  TARGET="$(uname -m)-unknown-linux-gnu" ;;
    *)      echo "these tests need macOS or Linux" >&2; exit 2 ;;
esac

VERSION="v0.1.0"
NAME="gabriel-$TARGET"
DIST="$WORK/dist/$VERSION"
mkdir -p "$DIST/$NAME"
cp "$BINARY" "$DIST/$NAME/gabriel"
tar czf "$DIST/$NAME.tar.gz" -C "$DIST" "$NAME"
rm -rf "$DIST/${NAME:?}"

sha256() {
    if command -v shasum >/dev/null; then shasum -a 256 "$1" | cut -d' ' -f1
    else sha256sum "$1" | cut -d' ' -f1; fi
}
sha256 "$DIST/$NAME.tar.gz" > "$DIST/$NAME.tar.gz.sha256"

# Keep pristine copies so a scenario that corrupts a fixture can restore it.
cp "$DIST/$NAME.tar.gz" "$WORK/pristine.tar.gz"
cp "$DIST/$NAME.tar.gz.sha256" "$WORK/pristine.sha256"
restore() {
    cp "$WORK/pristine.tar.gz" "$DIST/$NAME.tar.gz"
    cp "$WORK/pristine.sha256" "$DIST/$NAME.tar.gz.sha256"
}

python3 -m http.server "$PORT" --directory "$WORK/dist" >/dev/null 2>&1 &
SERVER_PID=$!
for _ in $(seq 1 40); do
    curl -fsS "http://127.0.0.1:$PORT/$VERSION/$NAME.tar.gz" -o /dev/null 2>/dev/null && break
    sleep 0.25
done

BASE="http://127.0.0.1:$PORT"
run_installer() {
    local dir="$1"; shift
    env GABRIEL_BASE_URL="$BASE" GABRIEL_VERSION="$VERSION" \
        GABRIEL_INSTALL_DIR="$dir" "$@" sh "$INSTALLER" >"$WORK/out.txt" 2>&1
    echo $?
}

echo "installer regression suite"
echo "  installer: $INSTALLER"
echo "  target:    $TARGET"

# ── 1. valid-download ───────────────────────────────────────────────────────

scenario "valid-download"
DIR="$WORK/install-valid"
code=$(run_installer "$DIR")
check "exits 0" "[ $code -eq 0 ]"
check "installs the binary" "[ -x '$DIR/gabriel' ]"
check "the binary runs" "'$DIR/gabriel' --version | grep -q gabriel"
check "reports the checksum was verified" "grep -q 'checksum verified' '$WORK/out.txt'"

scenario "valid-download (re-run is idempotent)"
code=$(run_installer "$DIR")
check "exits 0 again" "[ $code -eq 0 ]"
check "binary still runs" "'$DIR/gabriel' --version | grep -q gabriel"

# ── 2. checksum-failure ─────────────────────────────────────────────────────

scenario "checksum-failure"
printf 'tampered' >> "$DIST/$NAME.tar.gz"
DIR="$WORK/install-badsum"
code=$(run_installer "$DIR")
check "exits non-zero" "[ $code -ne 0 ]"
check "installs nothing" "[ ! -e '$DIR/gabriel' ]"
check "says the checksum did not match" "grep -qi 'checksum mismatch' '$WORK/out.txt'"
check "shows both digests" "[ \$(grep -cE 'expected|actual' '$WORK/out.txt') -ge 2 ]"
restore

# ── 3. 404-release ──────────────────────────────────────────────────────────

scenario "404-release"
DIR="$WORK/install-404"
code=$(env GABRIEL_BASE_URL="$BASE" GABRIEL_VERSION=v9.9.9 \
    GABRIEL_INSTALL_DIR="$DIR" sh "$INSTALLER" >"$WORK/out.txt" 2>&1; echo $?)
check "exits non-zero" "[ $code -ne 0 ]"
check "installs nothing" "[ ! -e '$DIR/gabriel' ]"
check "names the URL it tried" "grep -q 'v9.9.9' '$WORK/out.txt'"

# ── 4. unsupported-platform ─────────────────────────────────────────────────

scenario "unsupported-platform"
SHIM="$WORK/shim"
mkdir -p "$SHIM"
cat > "$SHIM/uname" <<'SHIM'
#!/bin/sh
case "$1" in
  -s) echo "Plan9" ;;
  -m) echo "pdp11" ;;
  *)  echo "Plan9" ;;
esac
SHIM
chmod +x "$SHIM/uname"
DIR="$WORK/install-platform"
code=$(env GABRIEL_BASE_URL="$BASE" GABRIEL_VERSION="$VERSION" \
    GABRIEL_INSTALL_DIR="$DIR" PATH="$SHIM:$PATH" sh "$INSTALLER" >"$WORK/out.txt" 2>&1; echo $?)
check "exits non-zero" "[ $code -ne 0 ]"
check "installs nothing" "[ ! -e '$DIR/gabriel' ]"
check "names the system it does not support" "grep -qi 'Plan9' '$WORK/out.txt'"
check "does not download anything first" "! grep -q 'checksum verified' '$WORK/out.txt'"

# ── 5. missing-path ─────────────────────────────────────────────────────────

scenario "missing-path"
DIR="$WORK/not-on-path"
code=$(env GABRIEL_BASE_URL="$BASE" GABRIEL_VERSION="$VERSION" \
    GABRIEL_INSTALL_DIR="$DIR" PATH="/usr/bin:/bin" sh "$INSTALLER" >"$WORK/out.txt" 2>&1; echo $?)
check "still installs" "[ -x '$DIR/gabriel' ]"
check "exits 0" "[ $code -eq 0 ]"
check "says the directory is not on PATH" "grep -q 'not on your PATH' '$WORK/out.txt'"
check "gives the export line to fix it" "grep -q 'export PATH' '$WORK/out.txt'"

# ── 6. permission-denied ────────────────────────────────────────────────────

scenario "permission-denied"
DIR="$WORK/readonly"
mkdir -p "$DIR"
chmod 555 "$DIR"
code=$(run_installer "$DIR")
check "exits non-zero" "[ $code -ne 0 ]"
check "installs nothing" "[ ! -e '$DIR/gabriel' ]"
check "says it cannot write there" "grep -qi 'cannot write' '$WORK/out.txt'"
check "suggests another directory" "grep -q 'GABRIEL_INSTALL_DIR' '$WORK/out.txt'"
chmod 755 "$DIR"

# ── 7. interrupted-download ─────────────────────────────────────────────────

scenario "interrupted-download"
# A server that promises more bytes than it sends, then hangs up — what a
# dropped connection looks like from the client side.
cat > "$WORK/truncating_server.py" <<'PY'
import http.server, socketserver, sys, pathlib

ARCHIVE = pathlib.Path(sys.argv[2])

class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    def log_message(self, *a): pass
    def do_GET(self):
        data = ARCHIVE.read_bytes()
        self.send_response(200)
        self.send_header("Content-Type", "application/octet-stream")
        # Claim the full length, deliver a third of it, then close.
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data[: len(data) // 3])
        self.wfile.flush()
        self.close_connection = True

socketserver.TCPServer.allow_reuse_address = True
with socketserver.TCPServer(("127.0.0.1", int(sys.argv[1])), Handler) as httpd:
    httpd.serve_forever()
PY
TRUNC_PORT=$((PORT + 1))
python3 "$WORK/truncating_server.py" "$TRUNC_PORT" "$WORK/pristine.tar.gz" >/dev/null 2>&1 &
TRUNC_PID=$!
sleep 1
DIR="$WORK/install-truncated"
code=$(env GABRIEL_BASE_URL="http://127.0.0.1:$TRUNC_PORT" GABRIEL_VERSION="$VERSION" \
    GABRIEL_INSTALL_DIR="$DIR" sh "$INSTALLER" >"$WORK/out.txt" 2>&1; echo $?)
kill "$TRUNC_PID" 2>/dev/null
check "exits non-zero" "[ $code -ne 0 ]"
check "installs nothing" "[ ! -e '$DIR/gabriel' ]"

# ── 8. corrupted-archive ────────────────────────────────────────────────────

scenario "corrupted-archive"
# Bytes that are not a tarball, with a checksum that matches them: the download
# is intact and still unusable, which checksums alone cannot catch.
printf 'this is not a gzip stream' > "$DIST/$NAME.tar.gz"
sha256 "$DIST/$NAME.tar.gz" > "$DIST/$NAME.tar.gz.sha256"
DIR="$WORK/install-corrupt"
code=$(run_installer "$DIR")
check "exits non-zero" "[ $code -ne 0 ]"
check "installs nothing" "[ ! -e '$DIR/gabriel' ]"
check "the checksum did pass first" "grep -q 'checksum verified' '$WORK/out.txt'"
restore

# ── 9. latest (the default path, no version given) ──────────────────────────

scenario "latest"
# What GitHub serves at /releases/latest/download when the API is unreachable,
# which is the fallback the installer has to survive.
mkdir -p "$WORK/dist/latest/download"
cp "$WORK/pristine.tar.gz" "$WORK/dist/latest/download/$NAME.tar.gz"
cp "$WORK/pristine.sha256" "$WORK/dist/latest/download/$NAME.tar.gz.sha256"
DIR="$WORK/install-latest"
code=$(env GABRIEL_BASE_URL="$BASE" GABRIEL_INSTALL_DIR="$DIR" \
    sh "$INSTALLER" >"$WORK/out.txt" 2>&1; echo $?)
check "exits 0 with no version given" "[ $code -eq 0 ]"
check "installs the binary" "[ -x '$DIR/gabriel' ]"
check "verified the checksum" "grep -q 'checksum verified' '$WORK/out.txt'"

# ── result ──────────────────────────────────────────────────────────────────

printf '\n%d passed, %d failed\n' "$PASSED" "$FAILED"
[ "$FAILED" -eq 0 ]
