#!/bin/sh
# Resolve the base commit for an event-range diff, root-safe.
#
# CI passes `github.event.before` (or the PR base) as the candidate. On a
# branch-creation event that value is the all-zero SHA, and on the first push
# of a clean-root history HEAD has NO parent at all — so the usual fallback
# `git rev-parse HEAD^` fails before any quality check has run. The rule:
# a real commit is used as-is; otherwise HEAD's parent when it has one;
# otherwise git's empty tree, so the diff covers everything HEAD introduced.
#
# Prints the resolved revision. Exit 0 always on a valid repository.
set -eu

candidate=${1:-}
empty_tree=$(git hash-object -t tree /dev/null)

if [ -n "$candidate" ] && git cat-file -e "${candidate}^{commit}" 2>/dev/null; then
    printf '%s\n' "$candidate"
elif git rev-parse --verify --quiet 'HEAD^{commit}' >/dev/null && git rev-parse --verify --quiet 'HEAD^' >/dev/null 2>&1; then
    git rev-parse 'HEAD^'
else
    printf '%s\n' "$empty_tree"
fi
