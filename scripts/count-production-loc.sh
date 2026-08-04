#!/bin/sh
set -eu
count=$(find src -type f -name '*.rs' -exec awk 'NF && $0 !~ /^[[:space:]]*\/\// { n++ } END { print n+0 }' {} \; | awk '{ n += $1 } END { print n+0 }')
printf '%s production lines\n' "$count"
test "$count" -le 4900
