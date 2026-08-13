#!/bin/sh
# Regression for moor issue #27: the Windows native SHA256SUMS must use LF line
# endings so the Linux release-candidate aggregation's `sha256sum -c SHA256SUMS`
# resolves exactly `moor.exe` and not `moor.exe\r`.
#
# This proves the CONSUMER contract (#27 outcome #3): GNU sha256sum resolves
# exactly moor.exe, not a normalized/arbitrary path, when the checksum file is
# canonical LF; and it reproduces #27's documented failure when the file is CRLF.
set -eu

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

cd "$work"

# 1. Dummy artifact + its real digest.
printf 'dummy moor binary contents\n' > moor.exe
digest="$(sha256sum moor.exe | cut -d' ' -f1)"

# 2. Canonical LF SHA256SUMS: "<digest>  moor.exe" + trailing LF.
printf '%s  moor.exe\n' "$digest" > SHA256SUMS

if ! out="$( cd "$work" && sha256sum -c SHA256SUMS 2>&1 )"; then
  echo "FAIL: canonical LF SHA256SUMS should verify, but sha256sum -c failed:" >&2
  echo "$out" >&2
  exit 1
fi

# Must report exactly "moor.exe: OK" — no trailing \r in the resolved name.
if ! printf '%s\n' "$out" | grep -q '^moor.exe: OK$'; then
  echo "FAIL: expected 'moor.exe: OK' from LF SHA256SUMS, got:" >&2
  echo "$out" >&2
  exit 1
fi

# 3. Same content but CRLF endings must FAIL (reproduces #27).
printf '%s  moor.exe\r\n' "$digest" > SHA256SUMS.crlf
if ( cd "$work" && sha256sum -c SHA256SUMS.crlf ) >/dev/null 2>&1; then
  echo "FAIL: CRLF SHA256SUMS unexpectedly verified; guard cannot distinguish CRLF from LF" >&2
  exit 1
fi

# 4. Success.
echo "sha256sums-portable: OK"
