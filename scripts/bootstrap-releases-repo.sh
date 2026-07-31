#!/bin/sh
# Seed the public releases repository with the two files a visitor needs before
# any release exists.
#
#   ./scripts/bootstrap-releases-repo.sh [git-url]
#
# Run from the root of the source repository. It pushes, so it uses *your*
# credentials — nothing here reads a token from the environment or invents one.
#
# Why a script rather than copying two files by hand: the release workflow syncs
# both of these on every release, so a hand-copied version that drifts would be
# served to users until the next tag. Copying them from source keeps the first
# impression and the released version the same file.

set -eu

REMOTE="${1:-git@github.com:Alban-M/gabriel-releases.git}"

[ -f install.sh ] || {
    echo "run this from the root of the Gabriel source repository" >&2
    exit 2
}
[ -f docs/releases-repo-README.md ] || {
    echo "docs/releases-repo-README.md is missing" >&2
    exit 2
}

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT INT TERM

echo "cloning $REMOTE"
if ! git clone --quiet "$REMOTE" "$work/repo" 2>/dev/null; then
    echo "could not clone — create the repository first, and make it PUBLIC." >&2
    echo "A private releases repository breaks every documented install path." >&2
    exit 1
fi

cp install.sh "$work/repo/install.sh"
chmod 755 "$work/repo/install.sh"
cp docs/releases-repo-README.md "$work/repo/README.md"
cp LICENSE "$work/repo/LICENSE" 2>/dev/null || true

cd "$work/repo"
git add install.sh README.md LICENSE 2>/dev/null || git add install.sh README.md

if git diff --cached --quiet; then
    echo "already up to date; nothing to push"
    exit 0
fi

git commit --quiet -m "Installer and landing page

Copied from the source repository so the first impression matches what the
release workflow syncs on every tag."
git push --quiet origin HEAD
echo "pushed install.sh and README.md"

echo
echo "Verify the way a stranger would — no token, no login:"
echo "  curl -fsSL https://raw.githubusercontent.com/Alban-M/gabriel-releases/main/install.sh | head -3"
