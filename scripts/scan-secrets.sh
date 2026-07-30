#!/bin/sh
# Scan files for credential-shaped text.
#
#   scripts/scan-secrets.sh NOTES.md [more files…]
#
# The Rust surfaces are covered by `gabriel-testkit`, which asserts on known
# canaries and is exact. This covers the surfaces that are not Rust — release
# notes, the changelog, anything published — where there is nothing to inject a
# canary into and the only available check is shape.
#
# Shape matching cannot prove the absence of a secret. It is here because
# release notes are published permanently and a pasted token is the realistic
# mistake, not because it is a guarantee.

set -eu

[ $# -gt 0 ] || { echo "usage: scan-secrets.sh <file>…" >&2; exit 2; }

status=0

for file in "$@"; do
    [ -f "$file" ] || { echo "scan-secrets: no such file: $file" >&2; exit 2; }

    # A JWT: three base64url segments starting with the `{"alg"` header.
    if grep -qE 'eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.' "$file"; then
        echo "$file: contains something shaped like a JWT"
        status=1
    fi

    # A URL carrying userinfo — `https://user:password@host`.
    if grep -qE '://[A-Za-z0-9._%-]+:[^@/[:space:]]+@' "$file"; then
        echo "$file: contains a URL with embedded credentials"
        status=1
    fi

    # Vendor-prefixed keys that are unambiguous by construction.
    if grep -qE '(sk|pk|rk)-(live|test)-[A-Za-z0-9]{16,}|ghp_[A-Za-z0-9]{20,}|xox[baprs]-[A-Za-z0-9-]{10,}|AKIA[0-9A-Z]{16}' "$file"; then
        echo "$file: contains something shaped like an API key"
        status=1
    fi

    # `Authorization: Bearer <token>` with a real-looking token after it. The
    # documented example uses a placeholder, which is the point of the length
    # bound.
    if grep -qEi 'bearer[[:space:]]+[A-Za-z0-9._-]{24,}' "$file"; then
        echo "$file: contains a Bearer token"
        status=1
    fi
done

if [ "$status" -eq 0 ]; then
    echo "scan-secrets: nothing credential-shaped in $*"
fi
exit "$status"
