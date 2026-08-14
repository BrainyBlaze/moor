#!/bin/sh
# Refuse retired normative surfaces in the frozen specification documents.
#
# Revision 4 retired a set of normative statements: the wire-schema-3 naming,
# the terminal-state-before-ATTACH_ACK attach order, the v1 lifecycle body, a
# normative `ended:"terminated"` branch, and grammar table rows for the
# dash-spelled command tokens. Each of them was found ALIVE in the documents
# after the code had already moved on — a reviewer caught every one of these by
# hand. The digest gate cannot catch this class: it proves the documents were
# re-pinned, not that their content stopped saying the retired thing. So this
# guard drives the actual normative patterns.
#
# Retrospective mentions are legal and necessary (the documents explain WHAT
# was retired and why). The patterns below are chosen so retrospectives do not
# match: the `ended:"terminated"` rule requires "retired" or "removed" on any
# line that spells the branch, and the token rule matches grammar TABLE ROWS only,
# not prose lists.
#
# Exit 0 when clean, 1 with every offending line listed otherwise.
set -eu

docs="spec/moor-spec.md spec/moor-wire-schema.md"
status=0

fail() {
    status=1
    printf '%s\n' "$1"
    printf '%s\n' "$2" | sed 's/^/    /'
}

# 1. The current revision is wire-schema-4. The v3 name may survive only in
#    spec/README.md's revision-history note, never in the normative documents.
if hits=$(grep -nE 'wire v3|Wire v3|wire-schema-3' $docs); then
    fail "retired revision naming in a normative document:" "$hits"
fi

# 2. The v4 attach prefix is status-first: ATTACH_ACK, then TERMINAL_STATE.
if hits=$(grep -nF 'before `ATTACH_ACK`' $docs); then
    fail "retired attach order (terminal state before ATTACH_ACK):" "$hits"
fi

# 3. Lifecycle records are v2. A frozen key-order sentence or a JSON body that
#    pins the lifecycle version back to 1 is the retired shape.
if hits=$(grep -nE 'respectively `1`, `"lifecycle"`' $docs); then
    fail "v1 lifecycle key-order sentence:" "$hits"
fi
if hits=$(grep -nE '"v":1,"type":"lifecycle"' $docs); then
    fail "v1 lifecycle body:" "$hits"
fi

# 4. `ended:"terminated"` exists only as history. Any line spelling that branch
#    must say so — "retired" or "removed" on the same line.
if hits=$(grep -nF ':"terminated"' $docs | grep -vE 'retired|removed'); then
    fail 'normative ended:"terminated" branch (line lacks retired/removed):' "$hits"
fi

# 5. The dash-spelled command tokens have no grammar rows. A table row whose
#    FIRST cell is one of the nine retired spellings is normative grammar;
#    prose retrospectives never take that shape. `-a` as an OPTION cell of the
#    live `list`/`rm` rows sits in a later column and does not match.
if hits=$(grep -nE '^\|[[:space:]]*`-[aAcnNpkli]`[[:space:]]*(,|\|)' $docs); then
    fail "grammar table row for a retired dash token:" "$hits"
fi

# 6. The controller version byte is 04 in every frozen hex vector: the HELLO
#    magic `4D 4F 4F 52` may never be followed by the retired 03.
if hits=$(grep -nE '4D 4F 4F 52 03|MOOR\\x03' $docs); then
    fail "retired controller version byte in a frozen vector:" "$hits"
fi

# 7. The documents may not call themselves version 3, and the wire dialect is
#    never "wire schema 3" — history lives in one place, the spec README.
if hits=$(grep -nE 'version 3|Version 3|[Ww]ire schema 3' $docs); then
    fail "retired self-naming:" "$hits"
fi

# 8. `_SESSION_V2` is the sole ancestry carrier. A bare backticked legacy
#    key, or the exact retired dual-write/fallback phrasing, is the retired
#    arrangement. The OB-6 decision record legally DESCRIBES dual-writing in
#    its candidate table; these patterns are the normative spellings only.
if hits=$(grep -nE '\x60_SESSION\x60|falls back to the legacy carrier|writes both carriers|dual-writes' $docs); then
    fail "retired dual-carrier ancestry surface:" "$hits"
fi

exit $status
