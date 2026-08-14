#!/bin/sh
# Refuse retired normative surfaces in the frozen specification documents —
# and PROVE the refusal works by injecting every retired concept into a
# throwaway copy and requiring the guard to fail on it.
#
# Revision 4 retired a set of normative statements: wire-schema-3 naming and
# self-naming, the terminal-state-before-ATTACH_ACK attach order, the v1
# lifecycle body, a normative `ended:"terminated"` branch, grammar rows for
# the dash-spelled command tokens, the retired controller version byte in
# frozen vectors, and the dual-carrier ancestry surface. Each of them was
# found ALIVE in the documents after the code had moved on — every one caught
# by a reviewer, twice. The digest gate cannot catch this class: it proves
# the documents were re-pinned, not that their content stopped saying the
# retired thing. And a guard without negative controls can rot into false
# green, which also happened. So: the guard drives the patterns, and the
# self-test drives the guard.
#
# Retrospective mentions are legal and necessary (the documents explain WHAT
# was retired and why); the patterns are chosen so retrospectives do not
# match. Exit 0 when both the documents and the self-test are clean.
set -eu

# ---- the guard ------------------------------------------------------------
# check <dir>: scan the two documents under <dir>; print offenders, return 1.
check() {
    dir=$1
    docs="$dir/moor-spec.md $dir/moor-wire-schema.md"
    clean=0

    flag() {
        clean=1
        printf '%s\n' "$1"
        printf '%s\n' "$2" | sed 's/^/    /'
    }

    # 1. The current revision is wire-schema-4; v3 naming and version-3
    #    self-naming may survive only in spec/README.md's history note.
    if hits=$(grep -nE 'wire v3|Wire v3|wire-schema-3|version 3|Version 3|[Ww]ire schema 3' $docs); then
        flag "retired revision naming:" "$hits"
    fi

    # 2. The v4 attach prefix is status-first. Every retired spelling of the
    #    old order is refused: the before-ATTACH_ACK phrase, the
    #    preamble-then-ACK sequence in prose or path form, and coverage rows
    #    that refuse a POST-ACK preamble (v4 refuses a PRE-ACK one).
    if hits=$(grep -nE 'before `ATTACH_ACK`|before\*\* the attach acknowledgement|preamble/ACK|preamble.*, then `ATTACH_ACK`|post-`ATTACH_ACK` preamble|preamble was applied first|the following ACK' $docs); then
        flag "retired attach order:" "$hits"
    fi

    # 3. Lifecycle records are v2.
    if hits=$(grep -nE 'respectively `1`, `"lifecycle"`|"v":1,"type":"lifecycle"' $docs); then
        flag "v1 lifecycle:" "$hits"
    fi

    # 4. `ended:"terminated"` exists only as history: any line spelling the
    #    branch must say "retired" or "removed".
    if hits=$(grep -nF ':"terminated"' $docs | grep -vE 'retired|removed'); then
        flag 'normative ended:"terminated" branch:' "$hits"
    fi

    # 5. The dash-spelled command tokens have no grammar rows and no live
    #    acceptance prose. A table row whose FIRST cell is one of the nine
    #    retired spellings is normative grammar; "legacy command token" is
    #    live acceptance prose. `-a` as a later-column OPTION cell of the
    #    live `list`/`rm` rows does not match.
    if hits=$(grep -nE '^\|[[:space:]]*`-[aAcnNpkli]`[[:space:]]*(,|\|)|legacy command token' $docs); then
        flag "retired dash-token grammar:" "$hits"
    fi

    # 6. The controller version byte is 04 in every frozen hex vector.
    if hits=$(grep -nE '4D 4F 4F 52 03|MOOR\\x03' $docs); then
        flag "retired controller version byte:" "$hits"
    fi

    # 7. `_SESSION_V2` is the sole ancestry carrier: no bare backticked
    #    legacy key, no live dual-write/fallback contract, no
    #    carrier-agreement diagnostics. The OB-6 decision record legally
    #    DESCRIBES dual-writing inside its candidate table cells; "Dual-write
    #    the legacy" is the retired resolution spelling.
    if hits=$(grep -nE '`_SESSION`|falls back to the legacy carrier|writes both carriers|dual-writes|Dual-write the legacy|ancestry carriers disagree' $docs); then
        flag "retired dual-carrier ancestry surface:" "$hits"
    fi

    return $clean
}

# ---- the documents --------------------------------------------------------
status=0
check spec || status=1

# ---- the self-test: every retired concept must trip the guard -------------
# A guard that silently stops rejecting something must fail this script, not
# pass it. Each injection is one line appended to a throwaway copy.
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

injections="wire-schema-3 is the current revision
the schema is at version 3 today
TERMINAL_STATE is sent before \`ATTACH_ACK\` on attach
It is sent exactly once, and before** the attach acknowledgement.
the viewer receives the ordinary preamble/ACK/baseline
a missing, second or post-\`ATTACH_ACK\` preamble refused
the gate requires that the mandatory preamble was applied first
exactness is false, in which case the following ACK clears its bit
Values are respectively \`1\`, \`\"lifecycle\"\`, \`\"running\"\`
{\"v\":1,\"type\":\"lifecycle\",\"phase\":\"running\"}
Windows reports ended:\"terminated\" for a holder terminate
| \`-A\` | attach or create | — |
after a modern or legacy command token the options follow
4D 4F 4F 52 03 01 00 00
the program writes \`_SESSION\` for its ancestry
current falls back to the legacy carrier
the holder writes both carriers on every session
The holder dual-writes the legacy value
OB-6: Dual-write the legacy ancestry plus the versioned carrier
diagnostic: ancestry carriers disagree"

total=0
caught=0
printf '%s\n' "$injections" | while IFS= read -r line; do
    :
done
# POSIX sh: count in a file to survive the subshell.
printf '%s\n' "$injections" > "$work/cases"
while IFS= read -r line; do
    total=$((total + 1))
    cp spec/moor-spec.md "$work/moor-spec.md"
    cp spec/moor-wire-schema.md "$work/moor-wire-schema.md"
    printf '%s\n' "$line" >> "$work/moor-spec.md"
    if check "$work" >/dev/null 2>&1; then
        status=1
        printf 'self-test: guard MISSED the injected retirement: %s\n' "$line"
    else
        caught=$((caught + 1))
    fi
done < "$work/cases"
printf 'self-test: %s/%s injected retirements caught\n' "$caught" "$total"
[ "$caught" -eq "$total" ] || status=1

exit $status
