#!/bin/sh
# Proof that ci/check-tracked-paths.sh actually guards something.
#
# A guard is worth exactly as much as its demonstrated ability to FAIL. This
# reproduces the real incident first — a forbidden path force-added past an
# existing .gitignore rule, which is precisely how nine internal documents
# entered this repository's public history — and asserts the guard rejects it.
# Then it asserts a clean index passes, so the guard is not simply always-red.
# Finally it runs the guard against this repository, which is the check CI
# depends on.
set -eu

here="$(cd "$(dirname "$0")" && pwd)"
guard="$here/check-tracked-paths.sh"
repo="$(cd "$here/.." && pwd)"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# A throwaway repository so nothing below can touch the real index.
cd "$work"
git init --quiet .
git config user.email guard@test.invalid
git config user.name 'guard test'

# 1. The exact incident: the ignore rule is present AND the path is force-added.
printf 'docs/superpowers/\n' > .gitignore
mkdir -p docs/superpowers/plans
printf 'internal plan text\n' > docs/superpowers/plans/leak.md
git add .gitignore
git add -f docs/superpowers/plans/leak.md
git commit --quiet -m 'force-add past the ignore rule'

if out="$(sh "$guard" 2>&1)"; then
  echo "FAIL: the guard accepted a force-added docs/superpowers path." >&2
  echo "      An ignore rule does not stop 'git add -f', so a guard that" >&2
  echo "      passes here cannot have prevented the original leak." >&2
  echo "$out" >&2
  exit 1
fi

# The failure must NAME the path — an unattributed rejection is unactionable.
if ! printf '%s\n' "$out" | grep -q 'docs/superpowers/plans/leak\.md'; then
  echo "FAIL: the guard rejected the tree without naming the offending path:" >&2
  echo "$out" >&2
  exit 1
fi

# 2. A clean index must pass, or the guard is useless as a gate.
git rm -r --quiet --cached docs/superpowers
git commit --quiet -m 'untrack the internal documents'

if ! out="$(sh "$guard" 2>&1)"; then
  echo "FAIL: the guard rejected a clean index:" >&2
  echo "$out" >&2
  exit 1
fi

# Untracking is enough: the files may stay on disk, ignored.
if [ ! -f docs/superpowers/plans/leak.md ]; then
  echo "FAIL: the clean case removed the working-tree file; the guard is" >&2
  echo "      supposed to police the INDEX, not delete anyone's local notes." >&2
  exit 1
fi

# 3. The other half of the forbidden set. `.claude` is in the guard's pattern
# but was exercised by nothing, so until now only the docs/superpowers branch
# was actually known to work — an assertion about a pattern is not an
# assertion about the alternative inside it.
mkdir -p .claude
printf 'internal agent settings\n' > .claude/settings.json
git add -f .claude/settings.json
git commit --quiet -m 'force-add an agent configuration directory'

if out="$(sh "$guard" 2>&1)"; then
  echo "FAIL: the guard accepted a tracked .claude path." >&2
  echo "$out" >&2
  exit 1
fi

if ! printf '%s\n' "$out" | grep -q '\.claude/settings\.json'; then
  echo "FAIL: the guard rejected the tree without naming the .claude path:" >&2
  echo "$out" >&2
  exit 1
fi

git rm -r --quiet --cached .claude
git commit --quiet -m 'untrack the agent configuration directory'

# 4. The real repository, which is what CI runs this for.
cd "$repo"
if ! out="$(sh "$guard" 2>&1)"; then
  echo "FAIL: this repository tracks internal working documents:" >&2
  echo "$out" >&2
  exit 1
fi

echo "tracked-paths: OK"
