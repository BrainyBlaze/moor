#!/usr/bin/env python3
"""Create or verify the canonical manual-QA record for one Moor candidate."""

import argparse
import hashlib
import importlib.util
import json
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ASSEMBLER = os.path.join(HERE, "candidate-manifest.py")
spec = importlib.util.spec_from_file_location("candidate_manifest", ASSEMBLER)
candidate_manifest = importlib.util.module_from_spec(spec)
spec.loader.exec_module(candidate_manifest)

REPOSITORY = "https://github.com/BrainyBlaze/moor"
CHECKLIST = [
    "candidate-install",
    "binary-identity",
    "v4-dialect",
    "session-create",
    "provider-identity",
    "resume-argv",
    "resume-continuity",
    "resume-mismatch",
    "rebind",
    "channel-delivery",
    "input-path",
    "restart-geometry",
    "restart-adoption",
]

DECIMAL = re.compile(r"^[1-9][0-9]*$")
HEX40 = re.compile(r"^[0-9a-f]{40}$")
HEX64 = re.compile(r"^[0-9a-f]{64}$")
VERSION = re.compile(r"^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$")
TIMESTAMP = re.compile(r"^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$")
AUTHOR = re.compile(r"^[A-Za-z0-9](?:[A-Za-z0-9-]{0,37}[A-Za-z0-9])?$")
HOSTED_EVIDENCE = re.compile(
    r"^https://github\.com/BrainyBlaze/(?:moor|desk)/actions/runs/[1-9][0-9]*"
    r"(?:/job/[1-9][0-9]*)?$"
)


class Invalid(Exception):
    pass


def reject(message):
    raise Invalid(message)


def exact_keys(value, keys, what):
    if not isinstance(value, dict) or list(value) != keys:
        reject(f"{what} keys are {list(value) if isinstance(value, dict) else type(value).__name__}, expected {keys}")


def ascii_text(value, what, allow_empty=False):
    if not isinstance(value, str) or (not value and not allow_empty):
        reject(f"{what} is not a nonempty string")
    if not all(0x20 <= ord(char) <= 0x7E for char in value):
        reject(f"{what} is not printable ASCII")
    return value


def decimal(value, what):
    if not isinstance(value, str) or not DECIMAL.fullmatch(value):
        reject(f"{what} is not a nonzero decimal string")
    return value


def positive_int(value, what):
    if not isinstance(value, int) or isinstance(value, bool) or value <= 0:
        reject(f"{what} is not a positive integer")
    if value > 9007199254740991:
        reject(f"{what} exceeds 2**53-1")
    return value


def sha256(value, what):
    if not isinstance(value, str) or not HEX64.fullmatch(value):
        reject(f"{what} is not lowercase SHA-256")
    return value


def canonical(value):
    return (json.dumps(value, indent=2, ensure_ascii=True) + "\n").encode("ascii")


def duplicate_guard(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            reject(f"duplicate JSON key {key!r}")
        result[key] = value
    return result


def read_json(path, what, require_canonical=False):
    try:
        with open(path, "rb") as handle:
            body = handle.read()
        text = body.decode("ascii")
        value = json.loads(text, object_pairs_hook=duplicate_guard)
    except OSError as error:
        reject(f"cannot read {what}: {error}")
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        reject(f"invalid {what}: {error}")
    if require_canonical and body != canonical(value):
        reject(f"{what} is not canonical two-space JSON with one LF")
    return value, body


def validate_job(reference, what, run_id, attempt):
    exact_keys(reference, ["workflowRunId", "workflowRunAttempt", "jobId", "jobName"], what)
    if decimal(reference["workflowRunId"], f"{what}.workflowRunId") != run_id:
        reject(f"{what} cites another run")
    if positive_int(reference["workflowRunAttempt"], f"{what}.workflowRunAttempt") != attempt:
        reject(f"{what} cites another attempt")
    decimal(reference["jobId"], f"{what}.jobId")
    ascii_text(reference["jobName"], f"{what}.jobName")


def validate_manifest(value):
    exact_keys(
        value,
        ["schemaVersion", "repository", "version", "commit", "candidate", "coverage", "targets"],
        "manifest",
    )
    if value["schemaVersion"] != 1 or value["repository"] != REPOSITORY:
        reject("manifest fixed identity differs")
    if not isinstance(value["version"], str) or not VERSION.fullmatch(value["version"]):
        reject("manifest version is not stable vMAJOR.MINOR.PATCH")
    if not isinstance(value["commit"], str) or not HEX40.fullmatch(value["commit"]):
        reject("manifest commit is not 40 lowercase hex")
    exact_keys(value["candidate"], ["workflowRunId", "workflowRunAttempt", "metadataArtifactName"], "candidate")
    run_id = decimal(value["candidate"]["workflowRunId"], "candidate.workflowRunId")
    attempt = positive_int(value["candidate"]["workflowRunAttempt"], "candidate.workflowRunAttempt")
    if value["candidate"]["metadataArtifactName"] != "moor-release-candidate-v1":
        reject("candidate metadata artifact name differs")
    if value["coverage"] != {"requiredClosure": "full-matrix"}:
        reject("release QA v1 accepts only the current full-matrix closure")
    if not isinstance(value["targets"], dict) or list(value["targets"]) != candidate_manifest.TARGETS:
        reject("manifest target set/order differs")
    bare = value["version"][1:]
    for target in candidate_manifest.TARGETS:
        entry = value["targets"][target]
        exact_keys(
            entry,
            ["asset", "size", "sha256", "artifactId", "artifactName", "provenance"],
            target,
        )
        expected_asset = f"moor-{bare}-{candidate_manifest.ASSET_SUFFIX[target]}"
        if entry["asset"] != expected_asset:
            reject(f"{target} asset differs")
        positive_int(entry["size"], f"{target}.size")
        sha256(entry["sha256"], f"{target}.sha256")
        decimal(entry["artifactId"], f"{target}.artifactId")
        if entry["artifactName"] != f"moor-candidate-{target}":
            reject(f"{target} artifactName differs")
        exact_keys(entry["provenance"], ["build", "verification"], f"{target}.provenance")
        validate_job(entry["provenance"]["build"], f"{target}.build", run_id, attempt)
        verification = entry["provenance"]["verification"]
        if not isinstance(verification, list):
            reject(f"{target}.verification is not an array")
        observed = []
        for index, reference in enumerate(verification):
            exact_keys(
                reference,
                ["gate", "lane", "workflowRunId", "workflowRunAttempt", "jobId", "jobName"],
                f"{target}.verification[{index}]",
            )
            gate = ascii_text(reference["gate"], f"{target}.verification[{index}].gate")
            lane = ascii_text(reference["lane"], f"{target}.verification[{index}].lane")
            if gate not in candidate_manifest.GATE_ORDER:
                reject(f"{target} has unknown gate {gate}")
            validate_job(
                {key: reference[key] for key in ("workflowRunId", "workflowRunAttempt", "jobId", "jobName")},
                f"{target}.{gate}.{lane}",
                run_id,
                attempt,
            )
            observed.append((gate, lane))
        expected = [
            (gate, lane)
            for gate in candidate_manifest.GATE_ORDER
            for lane in sorted(candidate_manifest.REQUIRED[target].get(gate, set()))
        ]
        if observed != expected:
            reject(f"{target} verification closure/order differs")
    return value


def validate_candidate_record(value, manifest, metadata_id):
    exact_keys(
        value,
        [
            "repository",
            "version",
            "commit",
            "workflowRunId",
            "workflowRunAttempt",
            "metadataArtifactId",
            "metadataArtifactName",
            "targets",
        ],
        "candidate record",
    )
    candidate = manifest["candidate"]
    expected_scalars = {
        "repository": manifest["repository"],
        "version": manifest["version"],
        "commit": manifest["commit"],
        "workflowRunId": candidate["workflowRunId"],
        "workflowRunAttempt": candidate["workflowRunAttempt"],
        "metadataArtifactId": metadata_id,
        "metadataArtifactName": candidate["metadataArtifactName"],
    }
    for key, expected in expected_scalars.items():
        if value[key] != expected:
            reject(f"candidate record {key} differs")
    if not isinstance(value["targets"], list) or len(value["targets"]) != len(candidate_manifest.TARGETS):
        reject("candidate record target count differs")
    projection_keys = ["asset", "size", "sha256", "artifactId", "artifactName"]
    for index, target in enumerate(candidate_manifest.TARGETS):
        projection = value["targets"][index]
        exact_keys(projection, projection_keys, f"candidate record target {index}")
        expected = {key: manifest["targets"][target][key] for key in projection_keys}
        if projection != expected:
            reject(f"candidate record target {target} differs")


def validate_sums(body, manifest):
    expected = "".join(
        f'{entry["sha256"]}  {entry["asset"]}\n' for entry in manifest["targets"].values()
    ).encode("ascii")
    if body != expected:
        reject("SHA256SUMS bytes differ from the manifest")


def validate_evidence(value, manifest, metadata_id, record_id):
    exact_keys(
        value,
        ["schemaVersion", "repository", "version", "commit", "candidate", "platforms", "checklist", "confirmation"],
        "manual QA evidence",
    )
    for key, expected in (
        ("schemaVersion", 1),
        ("repository", manifest["repository"]),
        ("version", manifest["version"]),
        ("commit", manifest["commit"]),
    ):
        if value[key] != expected:
            reject(f"manual QA evidence {key} differs")
    exact_keys(
        value["candidate"],
        ["workflowRunId", "workflowRunAttempt", "metadataArtifactId", "candidateRecordArtifactId"],
        "manual QA candidate",
    )
    candidate = manifest["candidate"]
    if value["candidate"] != {
        "workflowRunId": candidate["workflowRunId"],
        "workflowRunAttempt": candidate["workflowRunAttempt"],
        "metadataArtifactId": metadata_id,
        "candidateRecordArtifactId": record_id,
    }:
        reject("manual QA candidate identity differs")
    if not isinstance(value["platforms"], list) or len(value["platforms"]) != 4:
        reject("manual QA platforms are not the exact four targets")
    platform_by_target = {}
    for index, item in enumerate(value["platforms"]):
        exact_keys(item, ["target", "verdict", "evidence"], f"platform {index}")
        target = item["target"]
        if target != candidate_manifest.TARGETS[index]:
            reject("manual QA platforms are missing, extra, or reordered")
        if item["verdict"] != "passed":
            reject(f"manual QA platform {target} did not pass")
        evidence = ascii_text(item["evidence"], f"platform {target} evidence")
        if not HOSTED_EVIDENCE.fullmatch(evidence):
            reject(f"platform {target} evidence is not a hosted Actions run or job URL")
        platform_by_target[target] = item
    if not isinstance(value["checklist"], list) or len(value["checklist"]) != len(CHECKLIST):
        reject("manual QA checklist has a missing or extra item")
    for index, item in enumerate(value["checklist"]):
        exact_keys(item, ["id", "verdict", "evidence"], f"checklist {index}")
        if item["id"] != CHECKLIST[index]:
            reject("manual QA checklist is missing, extra, or reordered")
        if item["verdict"] != "passed":
            reject(f"manual QA checklist item {item['id']} did not pass")
        evidence = ascii_text(item["evidence"], f"checklist {item['id']} evidence")
        if not HOSTED_EVIDENCE.fullmatch(evidence):
            reject(f"checklist {item['id']} evidence is not a hosted Actions run or job URL")
    expected_confirmation = (
        f"APPROVE MOOR {manifest['version']} {manifest['commit']} "
        f"{candidate['workflowRunId']}/{candidate['workflowRunAttempt']} "
        f"{metadata_id} {record_id} full-matrix"
    )
    if value["confirmation"] != expected_confirmation:
        reject("manual QA confirmation differs")
    return platform_by_target


def build_record(args):
    metadata_id = decimal(args.metadata_artifact_id, "metadata artifact ID")
    record_id = decimal(args.candidate_record_artifact_id, "candidate-record artifact ID")
    manifest, manifest_body = read_json(args.manifest, "manifest", require_canonical=True)
    validate_manifest(manifest)
    candidate_record, _ = read_json(args.candidate_record, "candidate record", require_canonical=True)
    validate_candidate_record(candidate_record, manifest, metadata_id)
    try:
        with open(args.sums, "rb") as handle:
            sums_body = handle.read()
    except OSError as error:
        reject(f"cannot read SHA256SUMS: {error}")
    validate_sums(sums_body, manifest)
    evidence, evidence_body = read_json(args.evidence_file, "manual QA evidence")
    platforms = validate_evidence(evidence, manifest, metadata_id, record_id)

    if args.evidence_author_association != "OWNER":
        reject("manual QA evidence author is not repository OWNER")
    if not AUTHOR.fullmatch(args.evidence_author):
        reject("manual QA evidence author is invalid")
    if not TIMESTAMP.fullmatch(args.evidence_created_at) or not TIMESTAMP.fullmatch(
        args.evidence_updated_at
    ):
        reject("manual QA evidence timestamps are not canonical UTC seconds")
    if args.evidence_created_at != args.evidence_updated_at:
        reject("manual QA evidence comment was edited")
    expected_url = (
        r"^https://github\.com/BrainyBlaze/moor/issues/[1-9][0-9]*#issuecomment-"
        + re.escape(args.evidence_comment_id)
        + r"$"
    )
    decimal(args.evidence_comment_id, "evidence comment ID")
    if not re.fullmatch(expected_url, args.evidence_url):
        reject("manual QA evidence URL is foreign or does not match the comment ID")

    targets = {}
    for target in candidate_manifest.TARGETS:
        source = manifest["targets"][target]
        targets[target] = {
            "asset": source["asset"],
            "size": source["size"],
            "sha256": source["sha256"],
            "artifactId": source["artifactId"],
            "artifactName": source["artifactName"],
            "manualQa": {
                "verdict": platforms[target]["verdict"],
                "evidence": platforms[target]["evidence"],
            },
        }
    candidate = manifest["candidate"]
    return {
        "schemaVersion": 1,
        "repository": manifest["repository"],
        "version": manifest["version"],
        "commit": manifest["commit"],
        "candidate": {
            "workflowRunId": candidate["workflowRunId"],
            "workflowRunAttempt": candidate["workflowRunAttempt"],
            "metadataArtifactId": metadata_id,
            "metadataArtifactName": candidate["metadataArtifactName"],
            "candidateRecordArtifactId": record_id,
            "candidateRecordArtifactName": "moor-release-candidate-record",
            "manifestSha256": hashlib.sha256(manifest_body).hexdigest(),
            "sha256sumsSha256": hashlib.sha256(sums_body).hexdigest(),
        },
        "coverage": manifest["coverage"],
        "targets": targets,
        "manualQa": {
            "verdict": "passed",
            "checklist": evidence["checklist"],
            "approvedBy": args.evidence_author,
            "approvedAt": args.evidence_created_at,
            "evidence": {
                "url": args.evidence_url,
                "commentId": args.evidence_comment_id,
                "createdAt": args.evidence_created_at,
                "updatedAt": args.evidence_updated_at,
                "file": "manual-qa-evidence.txt",
                "size": len(evidence_body),
                "sha256": hashlib.sha256(evidence_body).hexdigest(),
            },
            "confirmation": evidence["confirmation"],
        },
    }


def parser():
    root = argparse.ArgumentParser()
    subparsers = root.add_subparsers(dest="verb", required=True)
    for verb in ("create", "verify"):
        command = subparsers.add_parser(verb)
        command.add_argument("--manifest", required=True)
        command.add_argument("--sums", required=True)
        command.add_argument("--candidate-record", required=True)
        command.add_argument("--metadata-artifact-id", required=True)
        command.add_argument("--candidate-record-artifact-id", required=True)
        command.add_argument("--evidence-file", required=True)
        command.add_argument("--evidence-url", required=True)
        command.add_argument("--evidence-comment-id", required=True)
        command.add_argument("--evidence-author", required=True)
        command.add_argument("--evidence-author-association", required=True)
        command.add_argument("--evidence-created-at", required=True)
        command.add_argument("--evidence-updated-at", required=True)
        if verb == "create":
            command.add_argument("--out", required=True)
        else:
            command.add_argument("--qa-record", required=True)
    return root


def main():
    args = parser().parse_args()
    try:
        expected = canonical(build_record(args))
        if args.verb == "create":
            with open(args.out, "wb") as handle:
                handle.write(expected)
            print(f"wrote {args.out} ({len(expected)} bytes)")
            return
        try:
            with open(args.qa_record, "rb") as handle:
                actual = handle.read()
        except OSError as error:
            reject(f"cannot read QA record: {error}")
        if actual != expected:
            reject("QA record bytes differ from the exact reconstructed record")
        print("release QA record verified")
    except Invalid as error:
        print(f"release-qa-record: {error}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
