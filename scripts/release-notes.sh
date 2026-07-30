#!/bin/sh
# Print the CHANGELOG section for a version, for use as release notes.
#
#   scripts/release-notes.sh 0.1.0-preview.1
#   scripts/release-notes.sh v0.1.0-preview.1 path/to/CHANGELOG.md
#
# Exits non-zero if the version has no section. That is the point: it turns
# "released without writing down what changed" into a build failure rather than
# something noticed after the announcement.

set -eu

version="${1:?usage: release-notes.sh <version> [changelog]}"
changelog="${2:-CHANGELOG.md}"

# Accept the tag form or the Cargo form; the file uses one of them.
version="${version#v}"

[ -f "$changelog" ] || {
    printf 'release-notes: no such file: %s\n' "$changelog" >&2
    exit 1
}

# Print the lines between this version's heading and the next one, dropping the
# "unreleased" marker (a tagged build is, by construction, released) and the
# blank lines at either end.
section=$(awk -v want="$version" '
    /^## / {
        # Heading text with the leading "## " and any "v" prefix removed, cut at
        # the first space so "## v1.2.3 — title" still matches "1.2.3".
        heading = substr($0, 4)
        sub(/^v/, "", heading)
        split(heading, parts, " ")
        found = (parts[1] == want)
        if (found) { inside = 1; next }
        if (inside) { exit }
    }
    inside { print }
' "$changelog")

if [ -z "$(printf '%s' "$section" | tr -d '[:space:]')" ]; then
    printf 'release-notes: CHANGELOG.md has no section for %s\n' "$version" >&2
    printf 'Add a "## %s" heading describing the release before tagging.\n' "$version" >&2
    exit 1
fi

printf '%s\n' "$section" \
    | sed -e 's/ — unreleased$//' \
    | awk 'NF {blank = 0; started = 1} !NF {blank++} started && (!blank || blank == 1) {print}'
