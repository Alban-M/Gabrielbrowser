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

# HTTPS rather than SSH: the source repository already authenticates over
# HTTPS, so this uses the credential helper that is known to work here. Pass an
# SSH URL as the first argument if you prefer it.
REMOTE="${1:-https://github.com/Alban-M/gabriel-releases.git}"

# The install command fetches raw.githubusercontent.com/.../main/install.sh, so
# the branch name is not cosmetic. On an empty repository the branch you land on
# comes from init.defaultBranch, which is `master` on any machine that has not
# set it — and pushing that would leave the advertised URL a 404. Named here so
# it does not depend on whoever runs this.
BRANCH=main

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
if ! git clone "$REMOTE" "$work/repo" 2>"$work/clone.err"; then
    echo >&2
    echo "could not clone $REMOTE" >&2
    sed 's/^/  /' "$work/clone.err" >&2
    echo >&2
    # The reason is in the error above. Guessing one here would send somebody to
    # fix a setting that is already correct — an earlier version of this script
    # blamed repository visibility for what was actually a missing SSH key.
    echo "Common causes: the repository does not exist yet, or this machine" >&2
    echo "cannot authenticate to it. Check the message above before changing" >&2
    echo "anything." >&2
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
git branch -M "$BRANCH"
git push --quiet origin "$BRANCH"
echo "pushed install.sh and README.md to $BRANCH"

echo
echo "Verify the way a stranger would — no token, no login:"
echo "  curl -fsSL https://raw.githubusercontent.com/Alban-M/gabriel-releases/main/install.sh | head -3"
