#!/bin/sh
# Refuse to ship internal working documents.
#
# Nine plan/spec files from an internal agent workflow reached this repository's
# public history and stayed there through review. A .gitignore rule does not
# prevent that: `git add -f` overrides it silently, and once a path is tracked
# the ignore rule stops applying to it altogether. So the ignore rule states the
# intent and this guard enforces it, by asking git what is actually TRACKED
# rather than what happens to be present in the working tree.
#
# The forbidden set is one extended regular expression rather than a list of
# shell globs on purpose: an unquoted glob in a `for` list is expanded against
# the filesystem before it is ever used as a pattern, which silently turns
# `docs/superpowers/*` into whatever happens to exist under that directory and
# makes the guard pass. The companion test pins this behaviour.
#
# Exit 0 when the index is clean, 1 with the offending paths listed otherwise.
set -eu

# Anchored at a path boundary so `docs/superpowers` matches at the repository
# root and under any subdirectory, but a name that merely CONTAINS it does not.
forbidden='(^|/)(docs/superpowers|\.claude)/'

# grep exits 1 when nothing matches, which is the clean case here.
offenders="$(git ls-files | grep -E "$forbidden" || true)"

if [ -n "$offenders" ]; then
  echo "FAIL: internal working documents are tracked in this repository:" >&2
  printf '%s\n' "$offenders" >&2
  echo "" >&2
  echo "These belong outside the repository. Remove them from the index" >&2
  echo "(git rm -r --cached <path>) and keep them out of commits; the" >&2
  echo ".gitignore entry alone cannot stop a forced add." >&2
  exit 1
fi

echo "check-tracked-paths: OK"
