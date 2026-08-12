#!/bin/sh
set -eu
if grep -R -n --include='*.rs' '#\[rustfmt::skip\]' src 2>/dev/null; then
    echo 'rustfmt::skip is forbidden in production source' >&2
    exit 1
fi
cargo fmt --all -- --check
files=$(find . \( -path './.git' -o -path './target' -o -path './tests' -o -path './docs' -o -path './spec' \) -prune -o -type f \( -name '*.rs' -o -name '*.c' -o -name '*.h' -o -name '*.cpp' -o -name '*.m' -o -name '*.mm' -o -name '*.ps1' -o -name '*.sh' -o -name '*.py' \) ! -path './scripts/count-production-loc.sh' -print)
count=0
for file in $files; do
    case "$file" in
        *.sh|*.py|*.ps1) comment='^[[:space:]]*#' ;;
        *) comment='^[[:space:]]*//' ;;
    esac
    lines=$(awk -v comment="$comment" 'NF && ($0 !~ comment || (NR == 1 && $0 ~ /^[[:space:]]*#(!|requires)/)) { n++ } END { print n+0 }' "$file")
    printf '%5s %s\n' "$lines" "$file"
    count=$((count + lines))
done
printf '%5s TOTAL production lines\n' "$count"
