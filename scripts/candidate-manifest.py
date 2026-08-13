#!/usr/bin/env python3
"""Assemble the canonical moor-release-manifest-v1.json and SHA256SUMS.

Implements docs/release-manifest-v1.md exactly: exact key sets and order,
canonical JSON bytes (two-space indent, one space after ':', LF ending),
printable-ASCII-only strings without '"' or '\\', unsigned base-10 integers,
and the six-line SHA256SUMS in target-table order.

Closure is table-driven per docs/release-matrix.md: every required
(gate, lane) pair must be present as a green verification record whose
verified digest equals the build digest; a missing lane, an unknown lane,
a gate satisfied by the wrong lane, or a different-byte verification
refuses the whole manifest. Extra honest lanes are recorded but never
substitute for a required one.

Input: a directory of JSON records —
  build-<target>.json          one per target, written at the build boundary
  verify-<target>-<gate>-<lane>.json  one per green verification reference,
    carrying gate, lane, run/attempt/job identity, the verified sha256, and
    the exact source commit the verifying job checked out.
Environment: COMMIT, VERSION (with leading v), RUN_ID, RUN_ATTEMPT.
Output: moor-release-manifest-v1.json and SHA256SUMS in --out directory.
"""

import json
import os
import re
import sys

REPOSITORY = "https://github.com/BrainyBlaze/moor"

TARGETS = [
    "x86_64-unknown-linux-musl",
    "aarch64-unknown-linux-musl",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc",
    "aarch64-pc-windows-msvc",
]

ASSET_SUFFIX = {
    "x86_64-unknown-linux-musl": "linux-x64",
    "aarch64-unknown-linux-musl": "linux-arm64",
    "x86_64-apple-darwin": "macos-x64",
    "aarch64-apple-darwin": "macos-arm64",
    "x86_64-pc-windows-msvc": "windows-x64.exe",
    "aarch64-pc-windows-msvc": "windows-arm64.exe",
}

GATE_ORDER = ["native-conformance", "compatibility", "static-linkage", "identity"]

# docs/release-matrix.md, table-driven: the exact required (gate -> lanes)
# closure per target. Lane names are the canonical enrollment labels used by
# .github/workflows/native-self-hosted.yml so a record cannot cite a lane the
# frozen matrix does not assign to that target/gate.
#
# Windows x64: Server 2022 is the input-fidelity floor; Windows 10 1809 and
# Server 2019 are §12.8 native below-floor provenance (release-matrix.md), so
# they are native-conformance lanes, not compatibility. Linux compatibility
# spans Ubuntu/Alpine/WSL1/WSL2 on the exact bytes.
REQUIRED = {
    "x86_64-unknown-linux-musl": {
        "native-conformance": {"ubuntu-22.04-x64", "alpine-3.20-x64"},
        "compatibility": {"ubuntu-22.04-x64", "alpine-3.20-x64"},
        "static-linkage": {"ubuntu-22.04-x64"},
        "identity": {"ubuntu-22.04-x64"},
    },
    "aarch64-unknown-linux-musl": {
        "native-conformance": {"ubuntu-24.04-arm64", "alpine-3.20-arm64"},
        "compatibility": {"ubuntu-24.04-arm64", "alpine-3.20-arm64"},
        "static-linkage": {"ubuntu-24.04-arm64"},
        "identity": {"ubuntu-24.04-arm64"},
    },
    "x86_64-apple-darwin": {
        "native-conformance": {"macos-15-intel"},
        "compatibility": {"macos-15-intel"},
        "identity": {"macos-15-intel"},
    },
    "aarch64-apple-darwin": {
        "native-conformance": {"macos-15-arm64"},
        "compatibility": {"macos-15-arm64"},
        "identity": {"macos-15-arm64"},
    },
    "x86_64-pc-windows-msvc": {
        # Server 2022 is the input-fidelity floor. 1809 and Server 2019 are
        # §12.8 native below-floor provenance (release-matrix.md) and sit in
        # DEFERRED below until their self-hosted runners exist.
        "native-conformance": {"windows-2022-x64"},
        "compatibility": {"windows-2022-x64"},
        "static-linkage": {"windows-2022-x64"},
        "identity": {"windows-2022-x64"},
    },
    "aarch64-pc-windows-msvc": {
        "native-conformance": {"windows-11-arm64"},
        "compatibility": {"windows-11-arm64"},
        "static-linkage": {"windows-11-arm64"},
        "identity": {"windows-11-arm64"},
    },
}

# Lanes the frozen matrix assigns but that a v0.1.0 candidate does not have to
# carry, because no self-hosted runner is enrolled for them yet (operator
# decision of 2026-08-13: narrow the mandatory closure now, restore it once the
# runners exist). They stay PERMITTED, so the moment such a runner appears its
# record is accepted here with no code change — and every deferred lane a run
# could NOT verify is named in the manifest's own bytes, so a narrowed closure
# can never be mistaken for a full one.
DEFERRED = {
    "x86_64-unknown-linux-musl": {
        "compatibility": {"wsl1-ubuntu-22.04-x64", "wsl2-ubuntu-22.04-x64"},
    },
    "x86_64-pc-windows-msvc": {
        "native-conformance": {"windows-10-1809-x64", "windows-server-2019-x64"},
        "compatibility": {"windows-10-1809-x64", "windows-server-2019-x64"},
    },
}

# Every (target, gate, lane) the matrix permits — a record outside this exact
# set is refused (no "any globally known lane on any gate").
PERMITTED = {
    (target, gate, lane)
    for table in (REQUIRED, DEFERRED)
    for target, gates in table.items()
    for gate, lanes in gates.items()
    for lane in lanes
}

HEX64 = re.compile(r"^[0-9a-f]{64}$")
DECIMAL = re.compile(r"^[1-9][0-9]*$")
VERSION_RE = re.compile(r"^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$")
COMMIT_RE = re.compile(r"^[0-9a-f]{40}$")


def fail(message: str) -> None:
    print(f"candidate-manifest: {message}", file=sys.stderr)
    sys.exit(1)


def ascii_clean(value, what: str) -> str:
    if not isinstance(value, str):
        fail(f"{what} is not a string: {value!r}")
    if not all(0x20 <= ord(ch) <= 0x7E for ch in value):
        fail(f"{what} is not printable ASCII: {value!r}")
    if '"' in value or "\\" in value:
        fail(f'{what} contains a quote or backslash: {value!r}')
    return value


def decimal_string(value, what: str) -> str:
    if not isinstance(value, str) or not DECIMAL.fullmatch(value):
        fail(f"{what} is not a nonzero decimal string: {value!r}")
    return value


def positive_int(value, what: str) -> int:
    if not isinstance(value, int) or isinstance(value, bool) or value <= 0:
        fail(f"{what} is not a positive integer: {value!r}")
    if value > 9007199254740991:
        fail(f"{what} exceeds 2**53-1: {value!r}")
    return value


def job_reference(record: dict, what: str) -> dict:
    reference = {
        "workflowRunId": decimal_string(record.get("workflowRunId"), f"{what}.workflowRunId"),
        "workflowRunAttempt": positive_int(record.get("workflowRunAttempt"), f"{what}.workflowRunAttempt"),
        "jobId": decimal_string(record.get("jobId"), f"{what}.jobId"),
        "jobName": ascii_clean(record.get("jobName", ""), f"{what}.jobName"),
    }
    if not reference["jobName"]:
        fail(f"{what}.jobName is empty")
    return reference


def serialize(value, indent: int) -> str:
    pad = "  " * indent
    child = "  " * (indent + 1)
    if isinstance(value, dict):
        if not value:
            fail("empty object is not representable")
        members = ",\n".join(
            f'{child}"{key}": {serialize(item, indent + 1)}' for key, item in value.items()
        )
        return "{\n" + members + "\n" + pad + "}"
    if isinstance(value, list):
        if not value:
            fail("empty array is not representable")
        members = ",\n".join(f"{child}{serialize(item, indent + 1)}" for item in value)
        return "[\n" + members + "\n" + pad + "]"
    if isinstance(value, bool):
        fail("booleans do not appear in manifest v1")
    if isinstance(value, int):
        return str(value)
    if isinstance(value, str):
        return f'"{value}"'
    fail(f"unrepresentable value: {value!r}")


def load_json(path: str, what: str) -> dict:
    try:
        with open(path, encoding="ascii") as handle:
            value = json.load(handle)
    except OSError:
        fail(f"missing {what}: {path}")
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        fail(f"unreadable {what}: {error}")
    if not isinstance(value, dict):
        fail(f"{what} is not an object")
    return value


def main() -> None:
    if len(sys.argv) != 5 or sys.argv[1] != "--records" or sys.argv[3] != "--out":
        fail("usage: candidate-manifest.py --records <dir> --out <dir>")
    records_dir, out_dir = sys.argv[2], sys.argv[4]

    commit = os.environ.get("COMMIT", "")
    version = os.environ.get("VERSION", "")
    run_id = os.environ.get("RUN_ID", "")
    run_attempt_text = os.environ.get("RUN_ATTEMPT", "")

    if not COMMIT_RE.fullmatch(commit):
        fail(f"COMMIT is not 40 lowercase hex: {commit!r}")
    if not VERSION_RE.fullmatch(version):
        fail(f"VERSION is not vMAJOR.MINOR.PATCH: {version!r}")
    decimal_string(run_id, "RUN_ID")
    if not run_attempt_text.isdigit() or int(run_attempt_text) < 1:
        fail(f"RUN_ATTEMPT is not a positive integer: {run_attempt_text!r}")
    run_attempt = int(run_attempt_text)
    bare = version[1:]

    targets_object = {}
    unverified = []
    sums_lines = []
    for target in TARGETS:
        build = load_json(os.path.join(records_dir, f"build-{target}.json"), f"build record for {target}")

        asset = f"moor-{bare}-{ASSET_SUFFIX[target]}"
        if build.get("asset") != asset:
            fail(f"{target}: build asset {build.get('asset')!r} != {asset!r}")
        artifact_name = f"moor-candidate-{target}"
        if build.get("artifactName") != artifact_name:
            fail(f"{target}: artifactName {build.get('artifactName')!r} != {artifact_name!r}")
        size = positive_int(build.get("size"), f"{target}.size")
        sha256 = build.get("sha256", "")
        if not isinstance(sha256, str) or not HEX64.fullmatch(sha256):
            fail(f"{target}: sha256 is not 64 lowercase hex")
        artifact_id = decimal_string(build.get("artifactId"), f"{target}.artifactId")

        build_reference = job_reference(build, f"{target}.build")
        if build_reference["workflowRunId"] != run_id or build_reference["workflowRunAttempt"] != run_attempt:
            fail(f"{target}: build reference run/attempt differs from candidate run")

        references = []
        seen = set()
        prefix = f"verify-{target}-"
        for name in sorted(os.listdir(records_dir)):
            if not (name.startswith(prefix) and name.endswith(".json")):
                continue
            verify = load_json(os.path.join(records_dir, name), f"verification record {name}")
            gate = verify.get("gate")
            if gate not in GATE_ORDER:
                fail(f"{target}: unknown gate {gate!r} in {name}")
            lane = ascii_clean(verify.get("lane", ""), f"{target}.{gate}.lane")
            if (target, gate, lane) not in PERMITTED:
                fail(f"{target}: lane {lane!r} is not a matrix lane for gate {gate!r} in {name}")
            if (gate, lane) in seen:
                fail(f"{target}: duplicate (gate, lane) ({gate}, {lane})")
            seen.add((gate, lane))
            if verify.get("commit") != commit:
                fail(f"{target}: {name} verified commit {verify.get('commit')!r} != {commit!r}")
            if verify.get("sha256") != sha256:
                fail(f"{target}: {name} verified different bytes ({verify.get('sha256')!r})")
            reference = {"gate": gate, "lane": lane}
            reference.update(job_reference(verify, f"{target}.{gate}.{lane}"))
            references.append(reference)

        for gate, lanes in REQUIRED[target].items():
            missing = lanes - {ref["lane"] for ref in references if ref["gate"] == gate}
            if missing:
                fail(f"{target}: gate {gate} is missing required lanes {sorted(missing)}")

        # A deferred lane is optional, never silent: whatever this run could not
        # verify is carried into the manifest verbatim.
        for gate, lanes in DEFERRED.get(target, {}).items():
            covered = {ref["lane"] for ref in references if ref["gate"] == gate}
            for lane in sorted(lanes - covered):
                unverified.append({"target": target, "gate": gate, "lane": lane})

        references.sort(
            key=lambda ref: (
                GATE_ORDER.index(ref["gate"]),
                ref["lane"],
                int(ref["workflowRunId"]),
                ref["workflowRunAttempt"],
                int(ref["jobId"]),
            )
        )

        targets_object[target] = {
            "asset": ascii_clean(asset, "asset"),
            "size": size,
            "sha256": sha256,
            "artifactId": artifact_id,
            "artifactName": ascii_clean(artifact_name, "artifactName"),
            "provenance": {"build": build_reference, "verification": references},
        }
        sums_lines.append(f"{sha256}  {asset}\n")

    # Deterministic order, and never an empty array (serialize refuses one):
    # a full matrix states "full-matrix" and carries no unverified list at all.
    #
    # The label is derived from WHICH deferred pairs are missing, not merely
    # from whether any are: once a runner is enrolled for some deferred lanes
    # but not others, the candidate is no longer hosted-only, and a label that
    # still said so would be the one part of this object a reader trusts at a
    # glance while it was wrong.
    unverified.sort(key=lambda entry: (entry["target"], entry["gate"], entry["lane"]))
    deferred_total = sum(len(lanes) for gates in DEFERRED.values() for lanes in gates.values())
    if not unverified:
        closure = "full-matrix"
    elif len(unverified) == deferred_total:
        closure = "hosted-only"
    else:
        closure = "partial"
    coverage = {"requiredClosure": closure}
    if unverified:
        coverage["unverified"] = unverified

    manifest = {
        "schemaVersion": 1,
        "repository": REPOSITORY,
        "version": version,
        "commit": commit,
        "candidate": {
            "workflowRunId": run_id,
            "workflowRunAttempt": run_attempt,
            "metadataArtifactName": "moor-release-candidate-v1",
        },
        "coverage": coverage,
        "targets": targets_object,
    }

    body = serialize(manifest, 0) + "\n"
    sums = "".join(sums_lines)
    for text, what in ((body, "manifest"), (sums, "SHA256SUMS")):
        if "\r" in text:
            fail(f"{what} contains a carriage return")
        if not text.endswith("\n") or text.endswith("\n\n"):
            fail(f"{what} does not end in exactly one LF")

    os.makedirs(out_dir, exist_ok=True)
    manifest_path = os.path.join(out_dir, "moor-release-manifest-v1.json")
    with open(manifest_path, "w", encoding="ascii", newline="") as handle:
        handle.write(body)
    sums_path = os.path.join(out_dir, "SHA256SUMS")
    with open(sums_path, "w", encoding="ascii", newline="") as handle:
        handle.write(sums)

    print(f"wrote {manifest_path} ({len(body)} bytes) and {sums_path} ({len(sums)} bytes)")


if __name__ == "__main__":
    main()
