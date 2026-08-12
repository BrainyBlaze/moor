#!/usr/bin/env python3
"""Assemble the canonical moor-release-manifest-v1.json and SHA256SUMS.

Implements docs/release-manifest-v1.md exactly: exact key sets and order,
canonical JSON bytes (two-space indent, one space after ':', LF ending),
printable-ASCII-only strings without '"' or '\\', unsigned base-10 integers,
and the six-line SHA256SUMS in target-table order.

Input: a directory of JSON records —
  build-<target>.json          one per target, written at the build boundary
  verify-<target>-<gate>.json  one per green verification job reference
Environment: COMMIT, VERSION (with leading v), RUN_ID, RUN_ATTEMPT.
Output: moor-release-manifest-v1.json and SHA256SUMS in --out directory.

The script fails closed: a missing target, missing required gate, malformed
field, or non-canonical value is an error, never a weaker manifest.
"""

import json
import os
import re
import sys

REPOSITORY = "https://github.com/BrainyBlaze/moor"

# The exact six targets in canonical (table) order.
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

# release-matrix.md: gates that must be present per target. Linux requires
# explicit static-linkage evidence; every target requires native §12.8
# conformance and identity.
def required_gates(target: str) -> set:
    gates = {"native-conformance", "identity"}
    if "linux" in target:
        gates.add("static-linkage")
    return gates


HEX64 = re.compile(r"^[0-9a-f]{64}$")
DECIMAL = re.compile(r"^[1-9][0-9]*$")
VERSION_RE = re.compile(r"^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$")
COMMIT_RE = re.compile(r"^[0-9a-f]{40}$")


def fail(message: str) -> None:
    print(f"candidate-manifest: {message}", file=sys.stderr)
    sys.exit(1)


def ascii_clean(value: str, what: str) -> str:
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


def job_reference(record: dict, what: str, run_id: str, run_attempt: int) -> dict:
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


def main() -> None:
    if len(sys.argv) != 5 or sys.argv[1] != "--records" or sys.argv[3] != "--out":
        fail("usage: candidate-manifest.py --records <dir> --out <dir>")
    records_dir, out_dir = sys.argv[2], sys.argv[4]

    commit = os.environ.get("COMMIT", "")
    version = os.environ.get("VERSION", "")
    run_id = os.environ.get("RUN_ID", "")
    run_attempt_text = os.environ.get("RUN_ATTEMPT", "")
    metadata_name = "moor-release-candidate-v1"

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
    sums_lines = []
    for target in TARGETS:
        build_path = os.path.join(records_dir, f"build-{target}.json")
        try:
            with open(build_path, encoding="ascii") as handle:
                build = json.load(handle)
        except OSError:
            fail(f"missing build record for {target}")
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            fail(f"unreadable build record for {target}: {error}")

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

        build_reference = job_reference(build, f"{target}.build", run_id, run_attempt)
        if build_reference["workflowRunId"] != run_id or build_reference["workflowRunAttempt"] != run_attempt:
            fail(f"{target}: build reference run/attempt differs from candidate run")

        references = []
        seen = set()
        for name in sorted(os.listdir(records_dir)):
            prefix = f"verify-{target}-"
            if not (name.startswith(prefix) and name.endswith(".json")):
                continue
            with open(os.path.join(records_dir, name), encoding="ascii") as handle:
                verify = json.load(handle)
            gate = verify.get("gate")
            if gate not in GATE_ORDER:
                fail(f"{target}: unknown gate {gate!r} in {name}")
            lane = ascii_clean(verify.get("lane", ""), f"{target}.{gate}.lane")
            if not lane:
                fail(f"{target}: {gate} lane is empty")
            if (gate, lane) in seen:
                fail(f"{target}: duplicate (gate, lane) ({gate}, {lane})")
            seen.add((gate, lane))
            reference = {"gate": gate, "lane": lane}
            reference.update(job_reference(verify, f"{target}.{gate}", run_id, run_attempt))
            references.append(reference)

        present = {reference["gate"] for reference in references}
        missing = required_gates(target) - present
        if missing:
            fail(f"{target}: missing required gates {sorted(missing)}")

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

    manifest = {
        "schemaVersion": 1,
        "repository": REPOSITORY,
        "version": version,
        "commit": commit,
        "candidate": {
            "workflowRunId": run_id,
            "workflowRunAttempt": run_attempt,
            "metadataArtifactName": metadata_name,
        },
        "targets": targets_object,
    }

    body = serialize(manifest, 0) + "\n"
    os.makedirs(out_dir, exist_ok=True)
    manifest_path = os.path.join(out_dir, "moor-release-manifest-v1.json")
    with open(manifest_path, "w", encoding="ascii", newline="") as handle:
        handle.write(body)
    sums_path = os.path.join(out_dir, "SHA256SUMS")
    with open(sums_path, "w", encoding="ascii", newline="") as handle:
        handle.write("".join(sums_lines))

    print(f"wrote {manifest_path} ({len(body)} bytes) and {sums_path}")


if __name__ == "__main__":
    main()
