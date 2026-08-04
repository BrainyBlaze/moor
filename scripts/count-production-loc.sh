#!/bin/sh
set -eu
files=$(find . \( -path './.git' -o -path './target' -o -path './tests' -o -path './docs' -o -path './spec' \) -prune -o -type f \( -name '*.rs' -o -name '*.c' -o -name '*.h' -o -name '*.cpp' -o -name '*.m' -o -name '*.mm' -o -name '*.ps1' -o -name '*.sh' -o -name '*.py' \) ! -path './scripts/count-production-loc.sh' -print)
count=0
for file in $files; do
    lines=$(awk 'NF && $0 !~ /^[[:space:]]*(\/\/|#)/ { n++ } END { print n+0 }' "$file")
    printf '%5s %s\n' "$lines" "$file"
    count=$((count + lines))
done
printf '%5s TOTAL production lines\n' "$count"
test "$count" -le 4900
