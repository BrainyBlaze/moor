#!/bin/sh
# Prove ci/diff-base.sh on all three shapes: a real base, a non-commit base
# on a history with a parent, and a non-commit base on a ROOT commit.
set -eu

here=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

git init -q "$work/repo"
cd "$work/repo"
git config user.email t@example.invalid
git config user.name t
printf 'a\n' > a && git add a && git -c commit.gpgsign=false commit -q -m one
root=$(git rev-parse HEAD)
empty=$(git hash-object -t tree /dev/null)

# 1. Root commit, non-commit base (branch-creation zero SHA): empty tree.
got=$(sh "$here/ci/diff-base.sh" 0000000000000000000000000000000000000000)
[ "$got" = "$empty" ] || { echo "root+zero: expected empty tree, got $got" >&2; exit 1; }
# and the diff itself must succeed and cover the root's content
git diff --check "$got" HEAD

# 2. Root commit, empty base: empty tree.
got=$(sh "$here/ci/diff-base.sh" "")
[ "$got" = "$empty" ] || { echo "root+empty: expected empty tree, got $got" >&2; exit 1; }

printf 'b\n' > b && git add b && git -c commit.gpgsign=false commit -q -m two
# 3. Non-root, non-commit base: parent of HEAD.
got=$(sh "$here/ci/diff-base.sh" 0000000000000000000000000000000000000000)
[ "$got" = "$root" ] || { echo "child+zero: expected $root, got $got" >&2; exit 1; }

# 4. A real commit base is used verbatim.
got=$(sh "$here/ci/diff-base.sh" "$root")
[ "$got" = "$root" ] || { echo "real base: expected $root, got $got" >&2; exit 1; }

echo "diff-base: root-safe on all shapes"
