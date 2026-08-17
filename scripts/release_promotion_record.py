#!/usr/bin/env python3
"""Pure canonical contracts for Moor's local-admin release promotion.

The module deliberately performs no network or ``gh`` operations.  It builds
and validates byte-exact promotion manifests, marked issue-comment records,
accepted-comment snapshots, and local transaction-evidence inventories.
"""

import argparse
import base64
import binascii
import hashlib
import importlib.util
import json
import os
import re
import stat
import sys
from datetime import datetime, timedelta, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
CANDIDATE_MANIFEST_PATH = os.path.join(HERE, "candidate-manifest.py")
_candidate_spec = importlib.util.spec_from_file_location(
    "release_promotion_candidate_manifest", CANDIDATE_MANIFEST_PATH
)
candidate_manifest = importlib.util.module_from_spec(_candidate_spec)
_candidate_spec.loader.exec_module(candidate_manifest)
CANDIDATE_TARGETS = tuple(candidate_manifest.TARGETS)
EXPECTED_CANDIDATE_ARTIFACT_NAMES = tuple(
    sorted(
        (
            "moor-release-candidate-v1",
            "moor-release-candidate-record",
            *(f"moor-candidate-{target}" for target in CANDIDATE_TARGETS),
        ),
        key=lambda item: item.encode("ascii"),
    )
)

RELEASE_QA_RECORD_PATH = os.path.join(HERE, "release-qa-record.py")
_release_qa_spec = importlib.util.spec_from_file_location(
    "release_promotion_release_qa_record", RELEASE_QA_RECORD_PATH
)
release_qa_record = importlib.util.module_from_spec(_release_qa_spec)
_release_qa_spec.loader.exec_module(release_qa_record)

PREFLIGHT_MARKER = b"<!-- moor-release-preflight-v1 -->\n"
COMPLETION_MARKER = b"<!-- moor-release-completion-v1 -->\n"

CLOCK_SKEW_SECONDS = 5
COMMENT_FRESHNESS_SECONDS = 15 * 60
PREFLIGHT_WAIT_SECONDS = 15 * 60
COMPLETION_WAIT_SECONDS = 60 * 60
POLL_INTERVAL_SECONDS = 5
AMBIGUOUS_OBSERVATION_SECONDS = 30

CLOCK_SKEW = timedelta(seconds=CLOCK_SKEW_SECONDS)
COMMENT_FRESHNESS = timedelta(seconds=COMMENT_FRESHNESS_SECONDS)

MAX_SAFE_INTEGER = 9007199254740991
DECIMAL = re.compile(r"^[1-9][0-9]*$")
HEX40 = re.compile(r"^[0-9a-f]{40}$")
HEX64 = re.compile(r"^[0-9a-f]{64}$")
NONCE = re.compile(r"^[0-9a-f]{64}$")
REPOSITORY = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$")
ACTOR = re.compile(r"^[A-Za-z0-9](?:[A-Za-z0-9-]{0,37}[A-Za-z0-9])?$")
TIMESTAMP = re.compile(
    r"^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$"
)
VERSION = re.compile(r"^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$")
API_DIGEST = re.compile(r"^sha256:[0-9a-f]{64}$")


class Invalid(Exception):
    """The supplied bytes or bound values do not satisfy the contract."""


class Waiting(Exception):
    """No comment for the requested nonce has appeared yet."""


def reject(message):
    raise Invalid(message)


def exact_keys(value, keys, what):
    expected = set(keys)
    if not isinstance(value, dict) or set(value) != expected or len(value) != len(keys):
        actual = sorted(value) if isinstance(value, dict) else type(value).__name__
        reject(f"{what} keys are {actual}, expected exactly {sorted(expected)}")


def release_qa_exact_keys(value, keys, what):
    """Apply the release-QA producer's order-sensitive object contract."""

    try:
        release_qa_record.exact_keys(value, keys, what)
    except release_qa_record.Invalid as error:
        reject(str(error))


def duplicate_guard(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            reject(f"duplicate JSON key {key!r}")
        result[key] = value
    return result


def canonical_json(value: object) -> bytes:
    """Encode one JSON value as sorted compact ASCII plus exactly one LF."""

    try:
        text = json.dumps(
            value,
            sort_keys=True,
            separators=(",", ":"),
            ensure_ascii=True,
            allow_nan=False,
        )
    except (TypeError, ValueError) as error:
        reject(f"value is not strict JSON: {error}")
    return text.encode("ascii") + b"\n"


def parse_json_bytes(body: bytes, what: str):
    if not isinstance(body, bytes):
        reject(f"{what} is not bytes")
    try:
        return json.loads(
            body.decode("ascii"),
            object_pairs_hook=duplicate_guard,
            parse_constant=lambda value: reject(
                f"invalid {what}: non-JSON constant {value!r}"
            ),
        )
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        reject(f"invalid {what}: {error}")


def _require_ascii_json(value, what):
    if isinstance(value, str):
        if not value.isascii():
            reject(f"{what} contains non-ASCII text")
    elif isinstance(value, list):
        for item in value:
            _require_ascii_json(item, what)
    elif isinstance(value, dict):
        for key, item in value.items():
            if not isinstance(key, str) or not key.isascii():
                reject(f"{what} contains a non-ASCII key")
            _require_ascii_json(item, what)


def encode_comment(marker: bytes, value: dict) -> bytes:
    if marker not in (PREFLIGHT_MARKER, COMPLETION_MARKER):
        reject("unknown promotion comment marker")
    if not isinstance(value, dict):
        reject("promotion comment record is not an object")
    _require_ascii_json(value, "promotion comment record")
    return marker + canonical_json(value)


def decode_comment(marker: bytes, body: bytes, what: str) -> dict:
    if marker not in (PREFLIGHT_MARKER, COMPLETION_MARKER):
        reject("unknown promotion comment marker")
    if not isinstance(body, bytes) or not body.startswith(marker):
        reject(f"{what} does not begin with its exact marker at byte zero")
    value = parse_json_bytes(body[len(marker) :], what)
    if not isinstance(value, dict):
        reject(f"{what} is not a JSON object")
    _require_ascii_json(value, what)
    if body != marker + canonical_json(value):
        reject(f"{what} is not canonical sorted compact JSON with one LF")
    return value


def decimal(value, what):
    if not isinstance(value, str) or not DECIMAL.fullmatch(value):
        reject(f"{what} is not a nonzero decimal string")
    if int(value) > MAX_SAFE_INTEGER:
        reject(f"{what} exceeds 2**53-1")
    return value


def positive_int(value, what):
    if not isinstance(value, int) or isinstance(value, bool) or value <= 0:
        reject(f"{what} is not a positive integer")
    if value > MAX_SAFE_INTEGER:
        reject(f"{what} exceeds 2**53-1")
    return value


def nonnegative_int(value, what):
    if not isinstance(value, int) or isinstance(value, bool) or value < 0:
        reject(f"{what} is not a nonnegative integer")
    if value > MAX_SAFE_INTEGER:
        reject(f"{what} exceeds 2**53-1")
    return value


def attempt(value, what):
    if not isinstance(value, int) or isinstance(value, bool) or value != 1:
        reject(f"{what} must be integer 1")
    return value


def sha40(value, what):
    if not isinstance(value, str) or not HEX40.fullmatch(value):
        reject(f"{what} is not 40 lowercase hex")
    return value


def sha256(value, what):
    if not isinstance(value, str) or not HEX64.fullmatch(value):
        reject(f"{what} is not lowercase SHA-256")
    return value


def api_digest(value, what):
    if not isinstance(value, str) or not API_DIGEST.fullmatch(value):
        reject(f"{what} is not canonical sha256:<64-lowercase-hex>")
    return value


def nonce(value, what):
    if not isinstance(value, str) or not NONCE.fullmatch(value):
        reject(f"{what} is not 64 lowercase hex")
    return value


def repository(value, what):
    if not isinstance(value, str) or not REPOSITORY.fullmatch(value):
        reject(f"{what} is not an owner/repository slug")
    return value


def actor(value, what):
    if not isinstance(value, str) or not ACTOR.fullmatch(value):
        reject(f"{what} is not a GitHub login")
    return value


def timestamp(value, what):
    if not isinstance(value, str) or not TIMESTAMP.fullmatch(value):
        reject(f"{what} is not canonical UTC seconds")
    try:
        parsed = datetime.strptime(value, "%Y-%m-%dT%H:%M:%SZ").replace(
            tzinfo=timezone.utc
        )
    except ValueError as error:
        reject(f"{what} is not a real timestamp: {error}")
    if parsed.strftime("%Y-%m-%dT%H:%M:%SZ") != value:
        reject(f"{what} is not canonical UTC seconds")
    return parsed


def ascii_text(value, what, allow_empty=False):
    if not isinstance(value, str) or (not value and not allow_empty):
        reject(f"{what} is not a {'printable' if allow_empty else 'nonempty printable'} ASCII string")
    if not all(0x20 <= ord(char) <= 0x7E for char in value):
        reject(f"{what} is not printable ASCII")
    return value


def validate_settings_response(body: bytes) -> dict:
    """Validate the exact documented two-field immutable-settings response."""

    if not isinstance(body, bytes) or not body or len(body) > 4096:
        reject("immutable settings response has an invalid byte length")
    value = parse_json_bytes(body, "immutable settings response")
    exact_keys(value, ["enabled", "enforced_by_owner"], "immutable settings response")
    if value["enabled"] is not True:
        reject("immutable releases were not enabled")
    if not isinstance(value["enforced_by_owner"], bool):
        reject("immutable settings owner enforcement is not boolean")
    return value


def read_bytes(path, what):
    try:
        with open(path, "rb") as handle:
            return handle.read()
    except OSError as error:
        reject(f"cannot read {what}: {error}")


def write_bytes(path, body, what):
    try:
        with open(path, "wb") as handle:
            handle.write(body)
    except OSError as error:
        reject(f"cannot write {what}: {error}")


def legacy_canonical_json(value):
    return (json.dumps(value, indent=2, ensure_ascii=True) + "\n").encode("ascii")


def read_json(path, what, canonical_encoder=None):
    body = read_bytes(path, what)
    value = parse_json_bytes(body, what)
    if canonical_encoder is not None and body != canonical_encoder(value):
        reject(f"{what} bytes are not canonical")
    return value, body


def safe_name(value, what):
    ascii_text(value, what)
    if value in (".", "..") or value.startswith("."):
        reject(f"{what} is a hostile name")
    if "/" in value or "\\" in value:
        reject(f"{what} is not a single file name")
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]*", value):
        reject(f"{what} contains a forbidden character")
    return value


def validate_qa_projection(value, expected_repository):
    """Validate the exact canonical output contract of the QA producer."""

    release_qa_exact_keys(
        value,
        [
            "schemaVersion",
            "repository",
            "version",
            "commit",
            "candidate",
            "candidateQa",
            "qaRun",
            "coverage",
            "targets",
            "manualQa",
        ],
        "release QA record",
    )
    attempt(value["schemaVersion"], "release QA schema version")
    expected_qa_repository = f"https://github.com/{expected_repository}"
    if (
        value["repository"] != expected_qa_repository
        or value["repository"] != release_qa_record.REPOSITORY
    ):
        reject("release QA record cites another repository")
    version = value["version"]
    if not isinstance(version, str) or not VERSION.fullmatch(version):
        reject("release QA version is not stable vMAJOR.MINOR.PATCH")
    commit = sha40(value["commit"], "release QA candidate commit")

    candidate = value["candidate"]
    release_qa_exact_keys(
        candidate,
        [
            "workflowRunId",
            "workflowRunAttempt",
            "metadataArtifactId",
            "metadataArtifactName",
            "candidateRecordArtifactId",
            "candidateRecordArtifactName",
            "manifestSha256",
            "sha256sumsSha256",
        ],
        "release QA candidate",
    )
    decimal(candidate["workflowRunId"], "candidate run ID")
    attempt(candidate["workflowRunAttempt"], "candidate run attempt")
    decimal(candidate["metadataArtifactId"], "candidate metadata artifact ID")
    decimal(
        candidate["candidateRecordArtifactId"],
        "candidate-record artifact ID",
    )
    if candidate["metadataArtifactName"] != "moor-release-candidate-v1":
        reject("candidate metadata artifact name differs")
    if candidate["candidateRecordArtifactName"] != "moor-release-candidate-record":
        reject("candidate-record artifact name differs")
    sha256(candidate["manifestSha256"], "candidate manifest SHA-256")
    sha256(candidate["sha256sumsSha256"], "candidate SHA256SUMS SHA-256")

    candidate_qa = value["candidateQa"]
    release_qa_exact_keys(
        candidate_qa,
        [
            "workflowRunId",
            "workflowRunAttempt",
            "evidenceArtifactId",
            "evidenceArtifactName",
            "deskCommit",
        ],
        "release QA candidate-QA",
    )
    decimal(candidate_qa["workflowRunId"], "candidate-QA run ID")
    attempt(candidate_qa["workflowRunAttempt"], "candidate-QA run attempt")
    decimal(candidate_qa["evidenceArtifactId"], "candidate-QA artifact ID")
    if candidate_qa["evidenceArtifactName"] != "moor-release-candidate-qa-evidence":
        reject("candidate-QA artifact name differs")
    sha40(candidate_qa["deskCommit"], "candidate-QA Desk commit")

    qa_run = value["qaRun"]
    release_qa_exact_keys(
        qa_run,
        ["workflowRunId", "workflowRunAttempt"],
        "release QA producer run",
    )
    decimal(qa_run["workflowRunId"], "release QA run ID")
    attempt(qa_run["workflowRunAttempt"], "release QA run attempt")
    release_qa_exact_keys(
        value["coverage"], ["requiredClosure"], "release QA coverage"
    )
    if value["coverage"] != {"requiredClosure": "full-matrix"}:
        reject("release QA coverage is not the current full matrix")

    targets = value["targets"]
    if not isinstance(targets, dict) or list(targets) != list(CANDIDATE_TARGETS):
        reject("release QA record target set/order differs")
    artifact_ids = {
        candidate["metadataArtifactId"],
        candidate["candidateRecordArtifactId"],
    }
    artifact_names = {
        candidate["metadataArtifactName"],
        candidate["candidateRecordArtifactName"],
    }
    assets = set()
    target_artifacts = []
    bare_version = version[1:]
    for target in CANDIDATE_TARGETS:
        entry = targets[target]
        ascii_text(target, "release QA target")
        release_qa_exact_keys(
            entry,
            [
                "asset",
                "size",
                "sha256",
                "artifactId",
                "artifactName",
                "manualQa",
            ],
            f"release QA target {target}",
        )
        name = safe_name(entry["asset"], f"release QA target {target} asset")
        expected_asset = (
            f"moor-{bare_version}-{candidate_manifest.ASSET_SUFFIX[target]}"
        )
        if name != expected_asset:
            reject(f"release QA target {target} asset name differs")
        positive_int(entry["size"], f"release QA target {target} size")
        sha256(entry["sha256"], f"release QA target {target} SHA-256")
        artifact_id = decimal(
            entry["artifactId"], f"release QA target {target} artifact ID"
        )
        artifact_name = safe_name(
            entry["artifactName"], f"release QA target {target} artifact name"
        )
        if artifact_name != f"moor-candidate-{target}":
            reject(f"release QA target {target} artifact name differs")
        target_manual_qa = entry["manualQa"]
        release_qa_exact_keys(
            target_manual_qa,
            ["verdict", "evidence"],
            f"release QA target {target} manual QA",
        )
        if target_manual_qa["verdict"] != "passed":
            reject(f"release QA target {target} did not pass manual QA")
        evidence = ascii_text(
            target_manual_qa["evidence"],
            f"release QA target {target} manual QA evidence",
        )
        hosted_evidence = re.compile(
            r"^https://github\.com/BrainyBlaze/moor/actions/runs/"
            + re.escape(candidate_qa["workflowRunId"])
            + r"(?:/job/[1-9][0-9]*)?$"
        )
        if not hosted_evidence.fullmatch(evidence):
            reject(
                f"release QA target {target} evidence is not from the bound "
                "candidate-QA run"
            )
        if name in assets:
            reject("release QA target asset names are not unique")
        if artifact_id in artifact_ids or artifact_name in artifact_names:
            reject("release QA candidate artifact identities are not unique")
        assets.add(name)
        artifact_ids.add(artifact_id)
        artifact_names.add(artifact_name)
        target_artifacts.append((artifact_id, artifact_name))
    if len(artifact_ids) != 6 or len(artifact_names) != 6:
        reject("release QA candidate artifact set is not exactly six")
    if candidate_qa["evidenceArtifactId"] in artifact_ids:
        reject("candidate-QA artifact ID collides with a candidate artifact")
    if candidate_qa["evidenceArtifactName"] in artifact_names:
        reject("candidate-QA artifact name collides with a candidate artifact")
    manual_qa = value["manualQa"]
    release_qa_exact_keys(
        manual_qa,
        [
            "verdict",
            "checklist",
            "approvedBy",
            "approvedAt",
            "evidence",
            "confirmation",
        ],
        "release QA manual QA",
    )
    if manual_qa["verdict"] != "passed":
        reject("release QA record does not carry a passed manual-QA verdict")
    checklist = manual_qa["checklist"]
    if not isinstance(checklist, list) or len(checklist) != len(
        release_qa_record.CHECKLIST
    ):
        reject("release QA manual-QA checklist has a missing or extra item")
    for index, item in enumerate(checklist):
        release_qa_exact_keys(
            item,
            ["id", "verdict", "evidence"],
            f"release QA manual-QA checklist item {index}",
        )
        expected_id = release_qa_record.CHECKLIST[index]
        if item["id"] != expected_id:
            reject("release QA manual-QA checklist is missing, extra, or reordered")
        if item["verdict"] != "passed":
            reject(f"release QA manual-QA checklist item {expected_id} did not pass")
        evidence = ascii_text(
            item["evidence"],
            f"release QA manual-QA checklist item {expected_id} evidence",
        )
        if not hosted_evidence.fullmatch(evidence):
            reject(
                f"release QA manual-QA checklist item {expected_id} evidence is "
                "not from the bound candidate-QA run"
            )
    actor(manual_qa["approvedBy"], "release QA manual-QA approver")
    approved_at = timestamp(
        manual_qa["approvedAt"], "release QA manual-QA approval time"
    )
    evidence = manual_qa["evidence"]
    release_qa_exact_keys(
        evidence,
        [
            "url",
            "commentId",
            "createdAt",
            "updatedAt",
            "authorAssociation",
            "repositoryPermission",
            "file",
            "size",
            "sha256",
        ],
        "release QA manual-QA evidence",
    )
    comment_id = decimal(
        evidence["commentId"], "release QA manual-QA evidence comment ID"
    )
    expected_evidence_url = re.compile(
        r"^https://github\.com/"
        + re.escape(expected_repository)
        + r"/issues/[1-9][0-9]*#issuecomment-"
        + re.escape(comment_id)
        + r"$"
    )
    evidence_url = ascii_text(evidence["url"], "release QA manual-QA evidence URL")
    if not expected_evidence_url.fullmatch(evidence_url):
        reject("release QA manual-QA evidence URL is foreign or mismatched")
    created_at = timestamp(
        evidence["createdAt"], "release QA manual-QA evidence creation time"
    )
    updated_at = timestamp(
        evidence["updatedAt"], "release QA manual-QA evidence update time"
    )
    if created_at != updated_at:
        reject("release QA manual-QA evidence comment was edited")
    if approved_at != created_at:
        reject("release QA manual-QA approval time differs from its evidence comment")
    if evidence["authorAssociation"] not in release_qa_record.AUTHOR_ASSOCIATIONS:
        reject("release QA manual-QA evidence author association is invalid")
    if evidence["repositoryPermission"] != "admin":
        reject("release QA manual-QA evidence author was not an administrator")
    if evidence["file"] != "manual-qa-evidence.txt":
        reject("release QA manual-QA evidence file name differs")
    positive_int(evidence["size"], "release QA manual-QA evidence size")
    sha256(evidence["sha256"], "release QA manual-QA evidence SHA-256")
    expected_confirmation = (
        f"APPROVE MOOR {version} {commit} "
        f"{candidate['workflowRunId']}/{candidate['workflowRunAttempt']} "
        f"{candidate['metadataArtifactId']} {candidate['candidateRecordArtifactId']} "
        "full-matrix"
    )
    if manual_qa["confirmation"] != expected_confirmation:
        reject("release QA manual-QA confirmation differs")
    return {
        "version": version,
        "commit": commit,
        "candidate": candidate,
        "candidateQa": candidate_qa,
        "qaRun": qa_run,
        "targets": targets,
        "candidateArtifacts": [
            (candidate["metadataArtifactId"], candidate["metadataArtifactName"]),
            (
                candidate["candidateRecordArtifactId"],
                candidate["candidateRecordArtifactName"],
            ),
            *target_artifacts,
        ],
    }


def parse_artifact_api_digests(values, expected_ids):
    bindings = {}
    for item in values:
        if not isinstance(item, str) or item.count("=") != 1:
            reject("artifact API digest binding is not ID=sha256:<hex>")
        artifact_id, digest = item.split("=", 1)
        decimal(artifact_id, "artifact API digest ID")
        api_digest(digest, f"artifact {artifact_id} API digest")
        if artifact_id in bindings:
            reject(f"duplicate artifact API digest binding for {artifact_id}")
        bindings[artifact_id] = digest
    if len(expected_ids) != len(set(expected_ids)):
        reject("artifact IDs collide across promotion provenance roles")
    if set(bindings) != set(expected_ids):
        reject("artifact API digest bindings differ from the exact QA artifact set")
    return bindings


def validate_expected_assets(value, qa):
    if not isinstance(value, list) or len(value) != 6:
        reject("expected assets must contain exactly six entries")
    names = []
    for index, entry in enumerate(value):
        exact_keys(entry, ["name", "size", "sha256"], f"expected asset {index}")
        names.append(safe_name(entry["name"], f"expected asset {index} name"))
        nonnegative_int(entry["size"], f"expected asset {index} size")
        sha256(entry["sha256"], f"expected asset {index} SHA-256")
    if names != sorted(names, key=lambda item: item.encode("ascii")):
        reject("expected assets are not sorted by ASCII name")
    if len(names) != len(set(names)):
        reject("expected asset names are not unique")
    expected = {
        entry["asset"]: {
            "name": entry["asset"],
            "size": entry["size"],
            "sha256": entry["sha256"],
        }
        for entry in qa["targets"].values()
    }
    expected["moor-release-manifest-v1.json"] = {
        "name": "moor-release-manifest-v1.json",
        "sha256": qa["candidate"]["manifestSha256"],
    }
    expected["SHA256SUMS"] = {
        "name": "SHA256SUMS",
        "sha256": qa["candidate"]["sha256sumsSha256"],
    }
    if set(names) != set(expected):
        reject("expected asset names differ from the QA-approved six files")
    for entry in value:
        wanted = expected[entry["name"]]
        if entry["sha256"] != wanted["sha256"]:
            reject(f"expected asset {entry['name']} SHA-256 differs from release QA")
        if "size" in wanted and entry["size"] != wanted["size"]:
            reject(f"expected asset {entry['name']} size differs from release QA")
    return value


def expected_release_body(qa, qa_artifact_id):
    return (
        f"Source-Commit: {qa['commit']}\n"
        f"Candidate-Run: {qa['candidate']['workflowRunId']}/1\n"
        f"Promotion-Transaction: {qa['qaRun']['workflowRunId']}/1/{qa_artifact_id}"
    ).encode("ascii")


def validate_manifest_inputs(args):
    repository_name = repository(args.repository, "repository")
    promotion_run_id = decimal(args.promotion_run_id, "promotion run ID")
    if args.promotion_run_attempt != "1":
        reject("promotion run attempt must be decimal string 1")
    head = sha40(args.head_sha, "promotion head SHA")
    if args.mode != "promote":
        reject("promotion manifest mode must be promote")
    promotion_nonce = nonce(args.nonce, "promotion nonce")
    qa_run_id = decimal(args.qa_run_id, "release QA run ID")
    if args.qa_run_attempt != "1":
        reject("release QA run attempt must be decimal string 1")
    qa_artifact_id = decimal(args.qa_artifact_id, "release QA artifact ID")

    qa_value, _ = read_json(
        args.qa_record,
        "release QA record",
        canonical_encoder=release_qa_record.canonical,
    )
    qa = validate_qa_projection(qa_value, repository_name)
    if qa["qaRun"] != {
        "workflowRunId": qa_run_id,
        "workflowRunAttempt": 1,
    }:
        reject("release QA record cites another producer run")

    expected_assets, _ = read_json(
        args.expected_assets,
        "expected assets",
        canonical_encoder=canonical_json,
    )
    validate_expected_assets(expected_assets, qa)
    body = read_bytes(args.release_body, "release body")
    if body != expected_release_body(qa, qa_artifact_id):
        reject("release body differs from the deterministic three-line body")

    artifact_ids = [item[0] for item in qa["candidateArtifacts"]]
    artifact_ids += [qa["candidateQa"]["evidenceArtifactId"], qa_artifact_id]
    digests = parse_artifact_api_digests(args.artifact_api_digest, artifact_ids)
    return {
        "repository": repository_name,
        "promotionRunId": promotion_run_id,
        "headSha": head,
        "mode": args.mode,
        "nonce": promotion_nonce,
        "qaArtifactId": qa_artifact_id,
        "qa": qa,
        "assets": expected_assets,
        "releaseBody": body,
        "digests": digests,
    }


def build_manifest(args):
    inputs = validate_manifest_inputs(args)
    qa = inputs["qa"]
    candidate = qa["candidate"]
    candidate_qa = qa["candidateQa"]
    candidate_artifacts = [
        {
            "id": artifact_id,
            "name": name,
            "apiDigest": inputs["digests"][artifact_id],
        }
        for artifact_id, name in sorted(
            qa["candidateArtifacts"], key=lambda item: item[1].encode("ascii")
        )
    ]
    return {
        "schemaVersion": 1,
        "kind": "moor-release-promotion-manifest-v1",
        "repository": inputs["repository"],
        "promotion": {
            "workflowRunId": inputs["promotionRunId"],
            "workflowRunAttempt": 1,
            "headSha": inputs["headSha"],
            "mode": inputs["mode"],
            "nonce": inputs["nonce"],
        },
        "qa": {
            "candidateQa": {
                "workflowRunId": candidate_qa["workflowRunId"],
                "workflowRunAttempt": 1,
                "artifactId": candidate_qa["evidenceArtifactId"],
                "artifactName": candidate_qa["evidenceArtifactName"],
                "apiDigest": inputs["digests"][candidate_qa["evidenceArtifactId"]],
                "deskCommit": candidate_qa["deskCommit"],
            },
            "releaseQa": {
                "workflowRunId": qa["qaRun"]["workflowRunId"],
                "workflowRunAttempt": 1,
                "artifactId": inputs["qaArtifactId"],
                "artifactName": "moor-release-qa-v1",
                "apiDigest": inputs["digests"][inputs["qaArtifactId"]],
            },
        },
        "candidate": {
            "workflowRunId": candidate["workflowRunId"],
            "workflowRunAttempt": 1,
            "commit": qa["commit"],
            "artifacts": candidate_artifacts,
        },
        "release": {
            "version": qa["version"],
            "tag": qa["version"],
            "name": f"Moor {qa['version']}",
            "bodySha256": hashlib.sha256(inputs["releaseBody"]).hexdigest(),
        },
        "assets": inputs["assets"],
    }


def validate_artifact_projection(value, what):
    exact_keys(value, ["id", "name", "apiDigest"], what)
    decimal(value["id"], f"{what} ID")
    safe_name(value["name"], f"{what} name")
    api_digest(value["apiDigest"], f"{what} API digest")


def validate_promotion_artifact_roles(qa, candidate, what):
    """Require every immutable artifact role and globally distinct IDs."""

    artifacts = candidate["artifacts"]
    names = tuple(artifact["name"] for artifact in artifacts)
    if names != EXPECTED_CANDIDATE_ARTIFACT_NAMES:
        reject(f"{what} candidate artifact role/name bindings differ")
    ids = [artifact["id"] for artifact in artifacts]
    ids.extend(
        [qa["candidateQa"]["artifactId"], qa["releaseQa"]["artifactId"]]
    )
    if len(ids) != len(set(ids)):
        reject(f"{what} artifact IDs collide across provenance roles")


def validate_manifest(value, body=None, expected=None):
    exact_keys(
        value,
        [
            "schemaVersion",
            "kind",
            "repository",
            "promotion",
            "qa",
            "candidate",
            "release",
            "assets",
        ],
        "promotion manifest",
    )
    attempt(value["schemaVersion"], "promotion manifest schema version")
    if value["kind"] != "moor-release-promotion-manifest-v1":
        reject("promotion manifest kind differs")
    repository(value["repository"], "promotion manifest repository")

    promotion = value["promotion"]
    exact_keys(
        promotion,
        ["workflowRunId", "workflowRunAttempt", "headSha", "mode", "nonce"],
        "promotion manifest promotion",
    )
    decimal(promotion["workflowRunId"], "manifest promotion run ID")
    attempt(promotion["workflowRunAttempt"], "manifest promotion run attempt")
    sha40(promotion["headSha"], "manifest promotion head SHA")
    if promotion["mode"] != "promote":
        reject("manifest promotion mode must be promote")
    nonce(promotion["nonce"], "manifest promotion nonce")

    qa = value["qa"]
    exact_keys(qa, ["candidateQa", "releaseQa"], "promotion manifest QA")
    candidate_qa = qa["candidateQa"]
    exact_keys(
        candidate_qa,
        [
            "workflowRunId",
            "workflowRunAttempt",
            "artifactId",
            "artifactName",
            "apiDigest",
            "deskCommit",
        ],
        "promotion manifest candidate-QA",
    )
    decimal(candidate_qa["workflowRunId"], "manifest candidate-QA run ID")
    attempt(candidate_qa["workflowRunAttempt"], "manifest candidate-QA attempt")
    decimal(candidate_qa["artifactId"], "manifest candidate-QA artifact ID")
    if candidate_qa["artifactName"] != "moor-release-candidate-qa-evidence":
        reject("manifest candidate-QA artifact name differs")
    api_digest(candidate_qa["apiDigest"], "manifest candidate-QA API digest")
    sha40(candidate_qa["deskCommit"], "manifest candidate-QA Desk commit")
    release_qa = qa["releaseQa"]
    exact_keys(
        release_qa,
        [
            "workflowRunId",
            "workflowRunAttempt",
            "artifactId",
            "artifactName",
            "apiDigest",
        ],
        "promotion manifest release-QA",
    )
    decimal(release_qa["workflowRunId"], "manifest release-QA run ID")
    attempt(release_qa["workflowRunAttempt"], "manifest release-QA attempt")
    decimal(release_qa["artifactId"], "manifest release-QA artifact ID")
    if release_qa["artifactName"] != "moor-release-qa-v1":
        reject("manifest release-QA artifact name differs")
    api_digest(release_qa["apiDigest"], "manifest release-QA API digest")

    candidate = value["candidate"]
    exact_keys(
        candidate,
        ["workflowRunId", "workflowRunAttempt", "commit", "artifacts"],
        "promotion manifest candidate",
    )
    decimal(candidate["workflowRunId"], "manifest candidate run ID")
    attempt(candidate["workflowRunAttempt"], "manifest candidate attempt")
    sha40(candidate["commit"], "manifest candidate commit")
    artifacts = candidate["artifacts"]
    if not isinstance(artifacts, list) or len(artifacts) != 6:
        reject("manifest candidate artifacts are not exactly six entries")
    for index, artifact in enumerate(artifacts):
        validate_artifact_projection(artifact, f"manifest candidate artifact {index}")
    artifact_names = [artifact["name"] for artifact in artifacts]
    artifact_ids = [artifact["id"] for artifact in artifacts]
    if artifact_names != sorted(artifact_names, key=lambda item: item.encode("ascii")):
        reject("manifest candidate artifacts are not sorted by ASCII name")
    if len(set(artifact_names)) != 6 or len(set(artifact_ids)) != 6:
        reject("manifest candidate artifact identities are not unique")
    validate_promotion_artifact_roles(qa, candidate, "manifest")

    release = value["release"]
    exact_keys(
        release,
        ["version", "tag", "name", "bodySha256"],
        "promotion manifest release",
    )
    if not isinstance(release["version"], str) or not VERSION.fullmatch(
        release["version"]
    ):
        reject("manifest release version is invalid")
    if release["tag"] != release["version"]:
        reject("manifest release tag differs from version")
    if release["name"] != f"Moor {release['version']}":
        reject("manifest release name differs")
    sha256(release["bodySha256"], "manifest release body SHA-256")
    validate_expected_asset_shape(value["assets"], "manifest")
    if body is not None and body != canonical_json(value):
        reject("promotion manifest is not canonical sorted compact JSON with one LF")
    if expected is not None and value != expected:
        reject("promotion manifest differs from the exact reconstructed manifest")
    return value


def validate_expected_asset_shape(value, what):
    if not isinstance(value, list) or len(value) != 6:
        reject(f"{what} assets are not exactly six entries")
    names = []
    for index, entry in enumerate(value):
        exact_keys(entry, ["name", "size", "sha256"], f"{what} asset {index}")
        names.append(safe_name(entry["name"], f"{what} asset {index} name"))
        nonnegative_int(entry["size"], f"{what} asset {index} size")
        sha256(entry["sha256"], f"{what} asset {index} SHA-256")
    if names != sorted(names, key=lambda item: item.encode("ascii")):
        reject(f"{what} assets are not sorted by ASCII name")
    if len(set(names)) != 6:
        reject(f"{what} asset names are not unique")


def validate_settings_binding(value, what):
    exact_keys(
        value,
        ["checkedAt", "responseBase64", "responseSha256"],
        what,
    )
    checked_at = timestamp(value["checkedAt"], f"{what} checked time")
    if not isinstance(value["responseBase64"], str):
        reject(f"{what} responseBase64 is not a string")
    try:
        response = base64.b64decode(value["responseBase64"], validate=True)
    except (binascii.Error, ValueError) as error:
        reject(f"{what} responseBase64 is invalid: {error}")
    if base64.b64encode(response).decode("ascii") != value["responseBase64"]:
        reject(f"{what} responseBase64 is not canonical")
    if (
        sha256(value["responseSha256"], f"{what} response SHA-256")
        != hashlib.sha256(response).hexdigest()
    ):
        reject(f"{what} response SHA-256 differs from the exact response bytes")
    validate_settings_response(response)
    return checked_at, response


def build_settings_binding(response_path, checked_at):
    timestamp(checked_at, "settings checked time")
    response = read_bytes(response_path, "immutable settings response")
    validate_settings_response(response)
    return {
        "checkedAt": checked_at,
        "responseBase64": base64.b64encode(response).decode("ascii"),
        "responseSha256": hashlib.sha256(response).hexdigest(),
    }


def validate_qa_manifest_projection(qa, candidate):
    exact_keys(qa, ["candidateQa", "releaseQa"], "record QA projection")
    candidate_qa = qa["candidateQa"]
    exact_keys(
        candidate_qa,
        [
            "workflowRunId",
            "workflowRunAttempt",
            "artifactId",
            "artifactName",
            "apiDigest",
            "deskCommit",
        ],
        "record candidate-QA projection",
    )
    decimal(candidate_qa["workflowRunId"], "record candidate-QA run ID")
    attempt(candidate_qa["workflowRunAttempt"], "record candidate-QA attempt")
    decimal(candidate_qa["artifactId"], "record candidate-QA artifact ID")
    if candidate_qa["artifactName"] != "moor-release-candidate-qa-evidence":
        reject("record candidate-QA artifact name differs")
    api_digest(candidate_qa["apiDigest"], "record candidate-QA API digest")
    sha40(candidate_qa["deskCommit"], "record candidate-QA Desk commit")

    release_qa = qa["releaseQa"]
    exact_keys(
        release_qa,
        [
            "workflowRunId",
            "workflowRunAttempt",
            "artifactId",
            "artifactName",
            "apiDigest",
        ],
        "record release-QA projection",
    )
    decimal(release_qa["workflowRunId"], "record release-QA run ID")
    attempt(release_qa["workflowRunAttempt"], "record release-QA attempt")
    decimal(release_qa["artifactId"], "record release-QA artifact ID")
    if release_qa["artifactName"] != "moor-release-qa-v1":
        reject("record release-QA artifact name differs")
    api_digest(release_qa["apiDigest"], "record release-QA API digest")

    exact_keys(
        candidate,
        ["workflowRunId", "workflowRunAttempt", "commit", "artifacts"],
        "record candidate projection",
    )
    decimal(candidate["workflowRunId"], "record candidate run ID")
    attempt(candidate["workflowRunAttempt"], "record candidate attempt")
    sha40(candidate["commit"], "record candidate commit")
    artifacts = candidate["artifacts"]
    if not isinstance(artifacts, list) or len(artifacts) != 6:
        reject("record candidate artifact projection is not exactly six entries")
    for index, artifact in enumerate(artifacts):
        validate_artifact_projection(artifact, f"record candidate artifact {index}")
    names = [artifact["name"] for artifact in artifacts]
    ids = [artifact["id"] for artifact in artifacts]
    if names != sorted(names, key=lambda item: item.encode("ascii")):
        reject("record candidate artifacts are not sorted by ASCII name")
    if len(set(names)) != 6 or len(set(ids)) != 6:
        reject("record candidate artifacts are not unique")
    validate_promotion_artifact_roles(qa, candidate, "record")


def validate_release_projection(value, candidate_commit):
    exact_keys(
        value,
        ["version", "tag", "name", "bodySha256"],
        "record release projection",
    )
    if not isinstance(value["version"], str) or not VERSION.fullmatch(
        value["version"]
    ):
        reject("record release version is invalid")
    if value["tag"] != value["version"]:
        reject("record release tag differs from version")
    if value["name"] != f"Moor {value['version']}":
        reject("record release name differs")
    sha256(value["bodySha256"], "record release body SHA-256")
    sha40(candidate_commit, "record release candidate commit")


def validate_record_promotion(value):
    exact_keys(
        value,
        [
            "workflowRunId",
            "workflowRunAttempt",
            "headSha",
            "mode",
            "nonce",
            "issueNumber",
            "gateReadyAt",
        ],
        "record promotion",
    )
    decimal(value["workflowRunId"], "record promotion run ID")
    attempt(value["workflowRunAttempt"], "record promotion attempt")
    sha40(value["headSha"], "record promotion head SHA")
    if value["mode"] != "promote":
        reject("record promotion mode must be promote")
    nonce(value["nonce"], "record promotion nonce")
    decimal(value["issueNumber"], "record promotion issue number")
    return timestamp(value["gateReadyAt"], "record gate-ready time")


def build_source(args, manifest):
    fields = (
        args.source_run_id,
        args.source_run_attempt,
        args.source_artifact_id,
        args.source_artifact_name,
        args.source_api_digest,
    )
    if args.source_mode == "run-bundle":
        if any(value is None for value in fields):
            reject("run-bundle source requires its complete run/artifact tuple")
        run_id = decimal(args.source_run_id, "run-bundle workflow run ID")
        if run_id != manifest["promotion"]["workflowRunId"]:
            reject("run-bundle source cites another promotion run")
        if args.source_run_attempt != "1":
            reject("run-bundle source attempt must be decimal string 1")
        artifact_id_text = decimal(
            args.source_artifact_id, "run-bundle artifact ID"
        )
        if args.source_artifact_name != "moor-release-promotion-v1":
            reject("run-bundle artifact name differs")
        api_digest(args.source_api_digest, "run-bundle API digest")
        return {
            "mode": "run-bundle",
            "workflowRunId": run_id,
            "workflowRunAttempt": 1,
            "artifactId": int(artifact_id_text),
            "artifactName": args.source_artifact_name,
            "apiDigest": args.source_api_digest,
        }
    if args.source_mode == "qa-reconstruction":
        if any(value is not None for value in fields):
            reject("qa-reconstruction source cannot carry run-bundle fields")
        return {
            "mode": "qa-reconstruction",
            "candidate": manifest["candidate"],
            "qa": manifest["qa"],
        }
    reject("source mode is not run-bundle or qa-reconstruction")


def validate_source(value, promotion, qa, candidate):
    if not isinstance(value, dict):
        reject("record source is not an object")
    mode = value.get("mode")
    if mode == "run-bundle":
        exact_keys(
            value,
            [
                "mode",
                "workflowRunId",
                "workflowRunAttempt",
                "artifactId",
                "artifactName",
                "apiDigest",
            ],
            "record run-bundle source",
        )
        if decimal(value["workflowRunId"], "record source run ID") != promotion[
            "workflowRunId"
        ]:
            reject("record source cites another promotion run")
        attempt(value["workflowRunAttempt"], "record source run attempt")
        positive_int(value["artifactId"], "record source artifact ID")
        if value["artifactName"] != "moor-release-promotion-v1":
            reject("record source artifact name differs")
        api_digest(value["apiDigest"], "record source API digest")
        return
    if mode == "qa-reconstruction":
        exact_keys(
            value,
            ["mode", "candidate", "qa"],
            "record QA-reconstruction source",
        )
        validate_qa_manifest_projection(value["qa"], value["candidate"])
        if value["candidate"] != candidate or value["qa"] != qa:
            reject("QA-reconstruction source tuple differs from the record")
        return
    reject("record source mode is not run-bundle or qa-reconstruction")


def load_promotion_manifest(path):
    value, body = read_json(path, "promotion manifest")
    validate_manifest(value, body=body)
    return value, body


def build_record_common(args):
    manifest, manifest_body = load_promotion_manifest(args.promotion_manifest)
    issue_number = decimal(args.issue_number, "promotion issue number")
    dispatcher = actor(args.dispatcher, "promotion dispatcher")
    administrator = actor(args.administrator, "current administrator")
    if dispatcher != administrator:
        reject("current administrator differs from the promotion dispatcher")
    gate_ready = timestamp(args.gate_ready_at, "gate-ready time")
    server_time = timestamp(args.github_server_time, "GitHub server time")
    settings = build_settings_binding(
        args.settings_response, args.settings_checked_at
    )
    checked_at, _ = validate_settings_binding(
        settings, "immutable release settings"
    )
    if abs(server_time - checked_at) > CLOCK_SKEW:
        reject("GitHub server time and settings check differ beyond clock skew")
    if checked_at - gate_ready < -CLOCK_SKEW:
        reject("immutable settings were checked before the gate was ready")
    if server_time - gate_ready < -CLOCK_SKEW:
        reject("GitHub server time predates the gate beyond clock skew")
    helper_commit = sha40(args.helper_commit, "helper commit")
    promotion = dict(manifest["promotion"])
    promotion.update(
        {"issueNumber": issue_number, "gateReadyAt": args.gate_ready_at}
    )
    return {
        "manifest": manifest,
        "manifestBody": manifest_body,
        "promotion": promotion,
        "source": build_source(args, manifest),
        "dispatcher": dispatcher,
        "administrator": administrator,
        "serverTime": args.github_server_time,
        "settings": settings,
        "helperCommit": helper_commit,
    }


def build_preflight(args):
    common = build_record_common(args)
    manifest = common["manifest"]
    return {
        "schemaVersion": 1,
        "kind": "moor-release-preflight-v1",
        "repository": manifest["repository"],
        "promotion": common["promotion"],
        "qa": manifest["qa"],
        "candidate": manifest["candidate"],
        "release": manifest["release"],
        "source": common["source"],
        "manifestSha256": hashlib.sha256(common["manifestBody"]).hexdigest(),
        "dispatcher": common["dispatcher"],
        "authority": {
            "administrator": common["administrator"],
            "gateReadyAt": common["promotion"]["gateReadyAt"],
            "githubServerTime": common["serverTime"],
            "immutableReleaseSettings": common["settings"],
        },
        "helperCommit": common["helperCommit"],
    }


def parse_cli_boolean(value, what):
    if value == "true":
        return True
    if value == "false":
        return False
    reject(f"{what} is not lowercase true or false")


def validate_published_assets(value, manifest_assets):
    if not isinstance(value, list) or len(value) != 6:
        reject("published assets are not exactly six entries")
    names = []
    ids = []
    projection = []
    for index, entry in enumerate(value):
        exact_keys(
            entry,
            ["id", "name", "size", "sha256"],
            f"published asset {index}",
        )
        ids.append(positive_int(entry["id"], f"published asset {index} ID"))
        names.append(safe_name(entry["name"], f"published asset {index} name"))
        nonnegative_int(entry["size"], f"published asset {index} size")
        sha256(entry["sha256"], f"published asset {index} SHA-256")
        projection.append(
            {
                "name": entry["name"],
                "size": entry["size"],
                "sha256": entry["sha256"],
            }
        )
    if names != sorted(names, key=lambda item: item.encode("ascii")):
        reject("published assets are not sorted by ASCII name")
    if len(set(names)) != 6 or len(set(ids)) != 6:
        reject("published asset names and IDs are not unique")
    if projection != manifest_assets:
        reject("published asset bytes differ from the promotion manifest")
    return value


def build_completion(args):
    common = build_record_common(args)
    manifest = common["manifest"]
    if args.authority_phase not in ("prepublish", "published-recovery"):
        reject("completion authority phase is invalid")
    preflight_id_text = decimal(
        args.preflight_comment_id, "accepted preflight comment ID"
    )
    expected_preflight_url = (
        f"https://github.com/{manifest['repository']}/issues/"
        f"{common['promotion']['issueNumber']}#issuecomment-{preflight_id_text}"
    )
    if args.preflight_comment_url != expected_preflight_url:
        reject("accepted preflight comment URL differs")
    preflight_body_sha = sha256(
        args.preflight_body_sha256, "accepted preflight body SHA-256"
    )
    version = manifest["release"]["version"]
    candidate_commit = manifest["candidate"]["commit"]
    if args.tag_ref != f"refs/tags/{version}":
        reject("published tag ref differs")
    if sha40(args.tag_sha, "published tag target SHA") != candidate_commit:
        reject("published tag cites another candidate")
    release_id_text = decimal(args.release_id, "published release ID")
    expected_release_url = (
        f"https://github.com/{manifest['repository']}/releases/tag/{version}"
    )
    if args.release_url != expected_release_url:
        reject("published release URL differs")
    for actual, wanted, what in (
        (args.release_tag, manifest["release"]["tag"], "tag"),
        (args.release_name, manifest["release"]["name"], "name"),
        (
            args.release_body_sha256,
            manifest["release"]["bodySha256"],
            "body SHA-256",
        ),
    ):
        if actual != wanted:
            reject(f"published release {what} differs")
    sha256(args.release_body_sha256, "published release body SHA-256")
    draft = parse_cli_boolean(args.release_draft, "published release draft flag")
    immutable = parse_cli_boolean(
        args.release_immutable, "published release immutable flag"
    )
    if draft is not False or immutable is not True:
        reject("completion requires a public immutable release")
    published_assets, _ = read_json(
        args.published_assets,
        "published assets",
        canonical_encoder=canonical_json,
    )
    validate_published_assets(published_assets, manifest["assets"])
    evidence_sha = sha256(
        args.transaction_evidence_manifest_sha256,
        "transaction evidence manifest SHA-256",
    )
    return {
        "schemaVersion": 1,
        "kind": "moor-release-completion-v1",
        "repository": manifest["repository"],
        "promotion": common["promotion"],
        "qa": manifest["qa"],
        "candidate": manifest["candidate"],
        "source": common["source"],
        "manifestSha256": hashlib.sha256(common["manifestBody"]).hexdigest(),
        "dispatcher": common["dispatcher"],
        "helperCommit": common["helperCommit"],
        "preflight": {
            "commentId": int(preflight_id_text),
            "commentUrl": args.preflight_comment_url,
            "bodySha256": preflight_body_sha,
        },
        "tag": {"ref": args.tag_ref, "targetSha": args.tag_sha},
        "release": {
            "id": int(release_id_text),
            "url": args.release_url,
            "version": version,
            "tag": args.release_tag,
            "name": args.release_name,
            "bodySha256": args.release_body_sha256,
            "targetCommit": candidate_commit,
            "draft": draft,
            "immutable": immutable,
        },
        "assets": published_assets,
        "authority": {
            "phase": args.authority_phase,
            "administrator": common["administrator"],
            "githubServerTime": common["serverTime"],
            "immutableReleaseSettings": common["settings"],
        },
        "transactionEvidenceManifestSha256": evidence_sha,
    }


def validate_authority_clock(gate_ready, server_time, checked_at):
    if abs(server_time - checked_at) > CLOCK_SKEW:
        reject("record GitHub server time and settings check exceed clock skew")
    if checked_at - gate_ready < -CLOCK_SKEW:
        reject("record settings check predates the gate beyond clock skew")
    if server_time - gate_ready < -CLOCK_SKEW:
        reject("record GitHub server time predates the gate beyond clock skew")


def validate_preflight(value, body=None, expected=None):
    exact_keys(
        value,
        [
            "schemaVersion",
            "kind",
            "repository",
            "promotion",
            "qa",
            "candidate",
            "release",
            "source",
            "manifestSha256",
            "dispatcher",
            "authority",
            "helperCommit",
        ],
        "preflight record",
    )
    attempt(value["schemaVersion"], "preflight schema version")
    if value["kind"] != "moor-release-preflight-v1":
        reject("preflight kind differs")
    repository_name = repository(value["repository"], "preflight repository")
    gate_ready = validate_record_promotion(value["promotion"])
    validate_qa_manifest_projection(value["qa"], value["candidate"])
    validate_release_projection(value["release"], value["candidate"]["commit"])
    validate_source(
        value["source"], value["promotion"], value["qa"], value["candidate"]
    )
    sha256(value["manifestSha256"], "preflight manifest SHA-256")
    dispatcher = actor(value["dispatcher"], "preflight dispatcher")
    authority = value["authority"]
    exact_keys(
        authority,
        [
            "administrator",
            "gateReadyAt",
            "githubServerTime",
            "immutableReleaseSettings",
        ],
        "preflight authority",
    )
    administrator = actor(
        authority["administrator"], "preflight current administrator"
    )
    if administrator != dispatcher:
        reject("preflight administrator differs from dispatcher")
    if authority["gateReadyAt"] != value["promotion"]["gateReadyAt"]:
        reject("preflight authority gate-ready time differs")
    timestamp(authority["gateReadyAt"], "preflight authority gate-ready time")
    server_time = timestamp(
        authority["githubServerTime"], "preflight GitHub server time"
    )
    checked_at, _ = validate_settings_binding(
        authority["immutableReleaseSettings"],
        "preflight immutable release settings",
    )
    validate_authority_clock(gate_ready, server_time, checked_at)
    sha40(value["helperCommit"], "preflight helper commit")
    expected_url_prefix = f"https://github.com/{repository_name}/"
    if not expected_url_prefix.startswith("https://github.com/"):
        reject("preflight repository URL binding is invalid")
    if body is not None and body != encode_comment(PREFLIGHT_MARKER, value):
        reject("preflight comment body is not exact canonical marked JSON")
    if expected is not None and value != expected:
        reject("preflight record differs from the exact reconstructed record")
    return value


def validate_completion(value, body=None, expected=None):
    exact_keys(
        value,
        [
            "schemaVersion",
            "kind",
            "repository",
            "promotion",
            "qa",
            "candidate",
            "source",
            "manifestSha256",
            "dispatcher",
            "helperCommit",
            "preflight",
            "tag",
            "release",
            "assets",
            "authority",
            "transactionEvidenceManifestSha256",
        ],
        "completion record",
    )
    attempt(value["schemaVersion"], "completion schema version")
    if value["kind"] != "moor-release-completion-v1":
        reject("completion kind differs")
    repository_name = repository(value["repository"], "completion repository")
    gate_ready = validate_record_promotion(value["promotion"])
    validate_qa_manifest_projection(value["qa"], value["candidate"])
    validate_source(
        value["source"], value["promotion"], value["qa"], value["candidate"]
    )
    sha256(value["manifestSha256"], "completion manifest SHA-256")
    dispatcher = actor(value["dispatcher"], "completion dispatcher")
    sha40(value["helperCommit"], "completion helper commit")

    preflight = value["preflight"]
    exact_keys(
        preflight,
        ["commentId", "commentUrl", "bodySha256"],
        "completion accepted preflight",
    )
    preflight_id = positive_int(
        preflight["commentId"], "completion accepted preflight comment ID"
    )
    expected_preflight_url = (
        f"https://github.com/{repository_name}/issues/"
        f"{value['promotion']['issueNumber']}#issuecomment-{preflight_id}"
    )
    if preflight["commentUrl"] != expected_preflight_url:
        reject("completion accepted preflight URL differs")
    sha256(preflight["bodySha256"], "completion accepted preflight body SHA-256")

    tag = value["tag"]
    exact_keys(tag, ["ref", "targetSha"], "completion tag")
    candidate_commit = value["candidate"]["commit"]
    if sha40(tag["targetSha"], "completion tag target SHA") != candidate_commit:
        reject("completion tag targets another candidate")

    release = value["release"]
    exact_keys(
        release,
        [
            "id",
            "url",
            "version",
            "tag",
            "name",
            "bodySha256",
            "targetCommit",
            "draft",
            "immutable",
        ],
        "completion release",
    )
    positive_int(release["id"], "completion release ID")
    if not isinstance(release["version"], str) or not VERSION.fullmatch(
        release["version"]
    ):
        reject("completion release version is invalid")
    if tag["ref"] != f"refs/tags/{release['version']}":
        reject("completion tag ref differs from the release version")
    if release["tag"] != release["version"]:
        reject("completion release tag differs from version")
    if release["name"] != f"Moor {release['version']}":
        reject("completion release name differs")
    if release["url"] != (
        f"https://github.com/{repository_name}/releases/tag/{release['version']}"
    ):
        reject("completion release URL differs")
    sha256(release["bodySha256"], "completion release body SHA-256")
    if (
        sha40(release["targetCommit"], "completion release target commit")
        != candidate_commit
    ):
        reject("completion release targets another candidate")
    if release["draft"] is not False or release["immutable"] is not True:
        reject("completion release is not public and immutable")

    assets = value["assets"]
    if not isinstance(assets, list) or len(assets) != 6:
        reject("completion assets are not exactly six entries")
    names = []
    ids = []
    for index, entry in enumerate(assets):
        exact_keys(
            entry,
            ["id", "name", "size", "sha256"],
            f"completion asset {index}",
        )
        ids.append(positive_int(entry["id"], f"completion asset {index} ID"))
        names.append(safe_name(entry["name"], f"completion asset {index} name"))
        nonnegative_int(entry["size"], f"completion asset {index} size")
        sha256(entry["sha256"], f"completion asset {index} SHA-256")
    if names != sorted(names, key=lambda item: item.encode("ascii")):
        reject("completion assets are not sorted by ASCII name")
    if len(set(names)) != 6 or len(set(ids)) != 6:
        reject("completion asset identities are not unique")

    authority = value["authority"]
    exact_keys(
        authority,
        [
            "phase",
            "administrator",
            "githubServerTime",
            "immutableReleaseSettings",
        ],
        "completion authority",
    )
    if authority["phase"] not in ("prepublish", "published-recovery"):
        reject("completion authority phase is invalid")
    administrator = actor(
        authority["administrator"], "completion current administrator"
    )
    if administrator != dispatcher:
        reject("completion administrator differs from dispatcher")
    server_time = timestamp(
        authority["githubServerTime"], "completion GitHub server time"
    )
    checked_at, _ = validate_settings_binding(
        authority["immutableReleaseSettings"],
        "completion immutable release settings",
    )
    validate_authority_clock(gate_ready, server_time, checked_at)
    sha256(
        value["transactionEvidenceManifestSha256"],
        "completion transaction evidence manifest SHA-256",
    )
    if body is not None and body != encode_comment(COMPLETION_MARKER, value):
        reject("completion comment body is not exact canonical marked JSON")
    if expected is not None and value != expected:
        reject("completion record differs from the exact reconstructed record")
    return value


def read_json_utf8(path, what):
    body = read_bytes(path, what)
    try:
        return json.loads(
            body.decode("utf-8"),
            object_pairs_hook=duplicate_guard,
            parse_constant=lambda value: reject(
                f"invalid {what}: non-JSON constant {value!r}"
            ),
        )
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        reject(f"invalid {what}: {error}")


def loose_record(body, marker):
    if not isinstance(body, str):
        return None
    marker_text = marker.decode("ascii")
    payloads = [body[len(marker_text) :]] if body.startswith(marker_text) else [body]
    if body.startswith("<!--") and "\n" in body:
        first_line, remainder = body.split("\n", 1)
        if first_line.endswith("-->"):
            payloads.append(remainder)
    for payload in payloads:
        try:
            value = json.loads(payload)
        except (json.JSONDecodeError, TypeError):
            continue
        if isinstance(value, dict):
            return value
    return None


def candidate_comments(comments, marker, kind, expected_nonce):
    candidates = []
    marker_text = marker.decode("ascii").rstrip("\n")
    for comment in comments:
        if not isinstance(comment, dict) or not isinstance(comment.get("body"), str):
            continue
        body = comment["body"]
        value = loose_record(body, marker)
        structurally_matches = False
        if isinstance(value, dict):
            promotion = value.get("promotion")
            structurally_matches = (
                value.get("kind") == kind
                and isinstance(promotion, dict)
                and promotion.get("nonce") == expected_nonce
            )
        text_matches = expected_nonce in body and (
            kind in body or marker_text in body
        )
        if structurally_matches or text_matches:
            candidates.append(comment)
    return candidates


def validate_comment_expectations(args, expected):
    expected_author = actor(args.expected_author, "expected comment author")
    if expected_author != expected["dispatcher"]:
        reject("expected comment author differs from the promotion dispatcher")
    timestamp(args.now, "comment verification time")
    if args.expected_comment_id is not None:
        decimal(args.expected_comment_id, "expected comment ID")
    if args.expected_body_sha256 is not None:
        sha256(args.expected_body_sha256, "expected comment body SHA-256")
    if args.final_recheck and (
        args.expected_comment_id is None or args.expected_body_sha256 is None
    ):
        reject("final recheck requires accepted comment ID and body SHA-256")


def verify_comment(args, marker, kind, expected, validator, what):
    validate_comment_expectations(args, expected)
    comments = read_json_utf8(args.comments, f"{what} comments")
    if isinstance(comments, dict):
        comments = [comments]
    if not isinstance(comments, list):
        reject(f"{what} comments are not an array or object")
    matches = candidate_comments(
        comments, marker, kind, expected["promotion"]["nonce"]
    )
    if not matches:
        raise Waiting(f"no matching {what} comment yet")
    if len(matches) != 1:
        reject(f"expected exactly one matching {what} comment, found {len(matches)}")
    comment = matches[0]
    try:
        body = comment["body"].encode("ascii")
    except (KeyError, AttributeError, UnicodeEncodeError) as error:
        reject(f"{what} comment body is invalid: {error}")
    value = decode_comment(marker, body, f"{what} comment body")
    validator(value, body=body, expected=expected)

    comment_id = comment.get("id")
    positive_int(comment_id, f"{what} comment ID")
    comment_id_text = str(comment_id)
    if (
        args.expected_comment_id is not None
        and comment_id_text != args.expected_comment_id
    ):
        reject(f"{what} comment ID changed")
    body_sha = hashlib.sha256(body).hexdigest()
    if (
        args.expected_body_sha256 is not None
        and body_sha != args.expected_body_sha256
    ):
        reject(f"{what} comment body SHA-256 changed")

    user = comment.get("user")
    if not isinstance(user, dict) or user.get("login") != args.expected_author:
        reject(f"{what} comment author differs")
    repository_name = expected["repository"]
    issue_number = expected["promotion"]["issueNumber"]
    expected_issue_url = (
        f"https://api.github.com/repos/{repository_name}/issues/{issue_number}"
    )
    expected_comment_url = (
        f"https://github.com/{repository_name}/issues/{issue_number}"
        f"#issuecomment-{comment_id_text}"
    )
    if comment.get("issue_url") != expected_issue_url:
        reject(f"{what} comment belongs to another issue")
    if comment.get("html_url") != expected_comment_url:
        reject(f"{what} comment URL is not canonical for its ID")
    if comment.get("created_at") != comment.get("updated_at"):
        reject(f"{what} comment was edited")

    gate_ready = timestamp(
        expected["promotion"]["gateReadyAt"], "expected gate-ready time"
    )
    created_at = timestamp(comment.get("created_at"), f"{what} creation time")
    now = timestamp(args.now, f"{what} verification time")
    authority = expected["authority"]
    checked_at = timestamp(
        authority["immutableReleaseSettings"]["checkedAt"],
        f"{what} settings checked time",
    )
    server_time = timestamp(
        authority["githubServerTime"], f"{what} GitHub server time"
    )
    if created_at - gate_ready < -CLOCK_SKEW:
        reject(f"{what} comment was created before the gate was ready")
    if checked_at - created_at > CLOCK_SKEW:
        reject(f"{what} settings check is later than the comment")
    if server_time - created_at > CLOCK_SKEW:
        reject(f"{what} GitHub server time is later than the comment")
    for observed, label in (
        (checked_at, "settings check"),
        (server_time, "GitHub server time"),
        (created_at, "comment"),
    ):
        if observed - now > CLOCK_SKEW:
            reject(f"{what} {label} is in the future")
    if not args.final_recheck:
        for observed, label in (
            (created_at, "comment"),
            (checked_at, "settings check"),
            (server_time, "GitHub server time"),
        ):
            if now - observed > COMMENT_FRESHNESS:
                reject(f"{what} {label} is stale")
    return {
        "schemaVersion": 1,
        "commentId": comment_id_text,
        "commentUrl": expected_comment_url,
        "author": args.expected_author,
        "createdAt": comment["created_at"],
        "updatedAt": comment["updated_at"],
        "bodySha256": body_sha,
        "record": value,
    }


def validate_evidence_path(value, what):
    if not isinstance(value, str) or not value:
        reject(f"{what} is not a nonempty string")
    if not value.isascii():
        reject(f"{what} is not ASCII")
    if value.startswith("/") or os.path.isabs(value):
        reject(f"{what} is absolute")
    if "\\" in value:
        reject(f"{what} contains a backslash")
    components = value.split("/")
    if any(component in ("", ".", "..") for component in components):
        reject(f"{what} contains an empty, dot, or dotdot component")
    for component in components:
        safe_name(component, f"{what} component")
    return value


def validate_evidence_manifest(value, body=None):
    exact_keys(
        value,
        ["schemaVersion", "kind", "files"],
        "transaction evidence manifest",
    )
    attempt(value["schemaVersion"], "transaction evidence schema version")
    if value["kind"] != "moor-release-transaction-evidence-manifest-v1":
        reject("transaction evidence manifest kind differs")
    files = value["files"]
    if not isinstance(files, list):
        reject("transaction evidence files are not an array")
    paths = []
    for index, entry in enumerate(files):
        exact_keys(
            entry,
            ["path", "size", "sha256"],
            f"transaction evidence file {index}",
        )
        paths.append(
            validate_evidence_path(
                entry["path"], f"transaction evidence file {index} path"
            )
        )
        nonnegative_int(entry["size"], f"transaction evidence file {index} size")
        sha256(entry["sha256"], f"transaction evidence file {index} SHA-256")
    if paths != sorted(paths, key=lambda item: item.encode("ascii")):
        reject("transaction evidence paths are not sorted by ASCII bytes")
    if len(paths) != len(set(paths)):
        reject("transaction evidence paths are not unique")
    if body is not None and body != canonical_json(value):
        reject("transaction evidence manifest bytes are not canonical")
    return value


def _same_file(left, right):
    return (left.st_dev, left.st_ino) == (right.st_dev, right.st_ino)


def _open_no_follow_at(directory_fd, name, flags, expected, what):
    open_flags = flags | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(name, open_flags, dir_fd=directory_fd)
    except OSError as error:
        reject(f"cannot open {what} without following links: {error}")
    try:
        observed = os.fstat(descriptor)
        if not _same_file(expected, observed):
            reject(f"{what} changed while being opened")
        return descriptor, observed
    except Exception:
        os.close(descriptor)
        raise


def _open_absolute_directory_no_links(path, what):
    absolute = os.path.abspath(path)
    directory_flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0)
    try:
        descriptor = os.open(os.path.sep, directory_flags)
    except OSError as error:
        reject(f"cannot open filesystem root for {what}: {error}")
    try:
        for component in (
            item for item in absolute.split(os.path.sep) if item
        ):
            try:
                expected = os.stat(
                    component,
                    dir_fd=descriptor,
                    follow_symlinks=False,
                )
            except OSError as error:
                reject(f"cannot inspect {what} path component: {error}")
            if stat.S_ISLNK(expected.st_mode):
                reject(f"{what} path contains a symlink")
            if not stat.S_ISDIR(expected.st_mode):
                reject(f"{what} path component is not a directory")
            child, observed = _open_no_follow_at(
                descriptor,
                component,
                directory_flags,
                expected,
                f"{what} directory",
            )
            if not stat.S_ISDIR(observed.st_mode):
                os.close(child)
                reject(f"{what} path component changed type")
            os.close(descriptor)
            descriptor = child
        return descriptor
    except Exception:
        os.close(descriptor)
        raise


def _open_bound_directory(path, what):
    absolute = os.path.abspath(path)
    try:
        expected = os.lstat(absolute)
    except OSError as error:
        reject(f"cannot inspect {what}: {error}")
    if stat.S_ISLNK(expected.st_mode):
        reject(f"{what} is a symlink")
    if not stat.S_ISDIR(expected.st_mode):
        reject(f"{what} is not a directory")
    descriptor = _open_absolute_directory_no_links(absolute, what)
    observed = os.fstat(descriptor)
    if not _same_file(expected, observed):
        os.close(descriptor)
        reject(f"{what} changed while being opened")
    return absolute, descriptor, observed


def _stat_at_optional(directory_fd, name, what):
    try:
        return os.stat(name, dir_fd=directory_fd, follow_symlinks=False)
    except FileNotFoundError:
        return None
    except OSError as error:
        reject(f"cannot inspect {what}: {error}")


def _validate_output_status(status, what):
    if stat.S_ISLNK(status.st_mode):
        reject(f"{what} is a symlink")
    if not stat.S_ISREG(status.st_mode):
        reject(f"{what} is not a regular file")
    if status.st_nlink != 1:
        reject(f"{what} has multiple hard links")


def _open_evidence_context(transaction_root, output_path):
    """Bind root, output parent, and any existing output before inventory."""

    root_absolute, root_fd, root_status = _open_bound_directory(
        transaction_root, "transaction root"
    )
    context = {"rootFd": root_fd}
    try:
        output_absolute = os.path.abspath(output_path)
        output_parent = os.path.dirname(output_absolute)
        output_name = os.path.basename(output_absolute)
        safe_name(output_name, "transaction evidence output name")
        parent_absolute, parent_fd, parent_status = _open_bound_directory(
            output_parent, "transaction evidence output parent"
        )
        context["parentFd"] = parent_fd
        output_status = _stat_at_optional(
            parent_fd, output_name, "transaction evidence output"
        )
        output_fd = None
        if output_status is not None:
            _validate_output_status(output_status, "transaction evidence output")
            output_fd, opened_output = _open_no_follow_at(
                parent_fd,
                output_name,
                os.O_RDONLY,
                output_status,
                "transaction evidence output",
            )
            context["outputFd"] = output_fd
            _validate_output_status(opened_output, "transaction evidence output")
        context.update(
            {
                "rootPath": root_absolute,
                "rootStatus": root_status,
                "parentPath": parent_absolute,
                "parentStatus": parent_status,
                "outputPath": output_absolute,
                "outputName": output_name,
                "outputStatus": output_status,
                "outputFd": output_fd,
                "outputRelative": None,
            }
        )
        try:
            if os.path.commonpath([root_absolute, output_absolute]) == root_absolute:
                relative = os.path.relpath(output_absolute, root_absolute).replace(
                    os.sep, "/"
                )
                context["outputRelative"] = validate_evidence_path(
                    relative, "transaction evidence output path"
                )
        except ValueError:
            pass
        return context
    except Exception:
        output_fd = context.get("outputFd")
        if output_fd is not None:
            os.close(output_fd)
        parent_fd = context.get("parentFd")
        if parent_fd is not None:
            os.close(parent_fd)
        os.close(root_fd)
        raise


def _close_evidence_context(context):
    for key in ("outputFd", "parentFd", "rootFd"):
        descriptor = context.get(key)
        if descriptor is not None:
            os.close(descriptor)
            context[key] = None


def _verify_directory_binding(path, expected, what):
    _, descriptor, observed = _open_bound_directory(path, what)
    try:
        if not _same_file(expected, observed):
            reject(f"{what} was replaced after inventory")
    finally:
        os.close(descriptor)


def _verify_output_binding(context):
    expected = context["outputStatus"]
    current = _stat_at_optional(
        context["parentFd"],
        context["outputName"],
        "transaction evidence output",
    )
    if expected is None:
        if current is not None:
            reject("transaction evidence output appeared after inventory began")
        return
    if current is None:
        reject("transaction evidence output disappeared after inventory began")
    _validate_output_status(current, "transaction evidence output")
    observed = os.fstat(context["outputFd"])
    _validate_output_status(observed, "bound transaction evidence output")
    if not _same_file(expected, current) or not _same_file(expected, observed):
        reject("transaction evidence output was substituted after inventory began")


def _verify_evidence_bindings(context):
    _verify_directory_binding(
        context["rootPath"], context["rootStatus"], "transaction root"
    )
    _verify_directory_binding(
        context["parentPath"],
        context["parentStatus"],
        "transaction evidence output parent",
    )
    _verify_output_binding(context)


def _inventory_evidence(context):
    output_status = context["outputStatus"]
    output_relative = context["outputRelative"]
    entries = []
    root_flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0)

    def inventory(directory_fd, relative_components):
        try:
            children = list(os.scandir(directory_fd))
        except OSError as error:
            reject(f"cannot scan transaction evidence: {error}")
        for child in children:
            relative = "/".join([*relative_components, child.name])
            validate_evidence_path(relative, "transaction evidence path")
            try:
                status = os.stat(
                    child.name,
                    dir_fd=directory_fd,
                    follow_symlinks=False,
                )
            except OSError as error:
                reject(f"cannot inspect transaction evidence {relative}: {error}")
            if stat.S_ISLNK(status.st_mode):
                reject(f"transaction evidence {relative} is a symlink")
            if stat.S_ISDIR(status.st_mode):
                child_fd, child_status = _open_no_follow_at(
                    directory_fd,
                    child.name,
                    root_flags,
                    status,
                    f"transaction evidence directory {relative}",
                )
                if not stat.S_ISDIR(child_status.st_mode):
                    os.close(child_fd)
                    reject(f"transaction evidence {relative} is no longer a directory")
                try:
                    inventory(child_fd, [*relative_components, child.name])
                finally:
                    os.close(child_fd)
                continue
            if not stat.S_ISREG(status.st_mode):
                reject(f"transaction evidence {relative} is not a regular file")
            if relative == output_relative:
                if output_status is None:
                    reject("transaction evidence output appeared during inventory")
                if not _same_file(status, output_status):
                    reject("transaction evidence output changed during inventory")
                continue
            if output_status is not None and _same_file(status, output_status):
                reject(
                    f"transaction evidence {relative} aliases the inventory output"
                )
            file_fd, file_status = _open_no_follow_at(
                directory_fd,
                child.name,
                os.O_RDONLY,
                status,
                f"transaction evidence file {relative}",
            )
            if not stat.S_ISREG(file_status.st_mode):
                os.close(file_fd)
                reject(f"transaction evidence {relative} is no longer a regular file")
            try:
                with os.fdopen(file_fd, "rb") as handle:
                    body = handle.read()
                    after_read = os.fstat(handle.fileno())
            except OSError as error:
                reject(f"cannot read transaction evidence {relative}: {error}")
            current = _stat_at_optional(
                directory_fd,
                child.name,
                f"transaction evidence {relative}",
            )
            stable_fields = ("st_size", "st_mtime_ns", "st_ctime_ns")
            if (
                current is None
                or not _same_file(file_status, after_read)
                or not _same_file(file_status, current)
                or any(
                    getattr(file_status, field) != getattr(after_read, field)
                    or getattr(file_status, field) != getattr(current, field)
                    for field in stable_fields
                )
                or len(body) != file_status.st_size
            ):
                reject(f"transaction evidence {relative} changed while being read")
            entries.append(
                {
                    "path": relative,
                    "size": len(body),
                    "sha256": hashlib.sha256(body).hexdigest(),
                }
            )

    inventory(context["rootFd"], [])
    entries.sort(key=lambda item: item["path"].encode("ascii"))
    value = {
        "schemaVersion": 1,
        "kind": "moor-release-transaction-evidence-manifest-v1",
        "files": entries,
    }
    validate_evidence_manifest(value)
    return value


def build_evidence_manifest(transaction_root, output_path):
    context = _open_evidence_context(transaction_root, output_path)
    try:
        return _inventory_evidence(context)
    finally:
        _close_evidence_context(context)


def _read_bound_output(context):
    descriptor = context["outputFd"]
    before = os.fstat(descriptor)
    try:
        os.lseek(descriptor, 0, os.SEEK_SET)
        chunks = []
        while True:
            chunk = os.read(descriptor, 1024 * 1024)
            if not chunk:
                break
            chunks.append(chunk)
    except OSError as error:
        reject(f"cannot read existing transaction evidence output: {error}")
    after = os.fstat(descriptor)
    if (
        not _same_file(before, after)
        or before.st_size != after.st_size
        or before.st_mtime_ns != after.st_mtime_ns
        or before.st_ctime_ns != after.st_ctime_ns
    ):
        reject("transaction evidence output changed while being compared")
    return b"".join(chunks)


def _new_evidence_temp(parent_fd, output_name):
    flags = (
        os.O_WRONLY
        | os.O_CREAT
        | os.O_EXCL
        | getattr(os, "O_NOFOLLOW", 0)
    )
    for _ in range(100):
        name = f".{output_name}.tmp-{os.getpid()}-{os.urandom(12).hex()}"
        try:
            descriptor = os.open(name, flags, 0o600, dir_fd=parent_fd)
        except FileExistsError:
            continue
        except OSError as error:
            reject(f"cannot create transaction evidence temporary output: {error}")
        status = os.fstat(descriptor)
        if not stat.S_ISREG(status.st_mode) or status.st_nlink != 1:
            os.close(descriptor)
            try:
                os.unlink(name, dir_fd=parent_fd)
            except OSError:
                pass
            reject("transaction evidence temporary output is not a private file")
        return name, descriptor
    reject("cannot allocate a unique transaction evidence temporary output")


def _write_all(descriptor, body):
    remaining = memoryview(body)
    while remaining:
        try:
            count = os.write(descriptor, remaining)
        except OSError as error:
            reject(f"cannot write transaction evidence manifest: {error}")
        if count <= 0:
            reject("cannot write complete transaction evidence manifest")
        remaining = remaining[count:]


def _commit_evidence_output(context, body):
    """Publish bytes without truncating or following any pre-existing path."""

    if not isinstance(body, bytes):
        reject("transaction evidence output body is not bytes")
    _verify_evidence_bindings(context)
    if context["outputStatus"] is not None:
        if _read_bound_output(context) != body:
            reject("existing transaction evidence output bytes differ")
        _verify_evidence_bindings(context)
        return

    parent_fd = context["parentFd"]
    name = context["outputName"]
    temp_name = None
    descriptor = None
    temp_status = None
    published = False
    committed = False
    try:
        temp_name, descriptor = _new_evidence_temp(parent_fd, name)
        _write_all(descriptor, body)
        os.fsync(descriptor)
        temp_status = os.fstat(descriptor)
        os.close(descriptor)
        descriptor = None
        _verify_evidence_bindings(context)
        try:
            os.link(
                temp_name,
                name,
                src_dir_fd=parent_fd,
                dst_dir_fd=parent_fd,
                follow_symlinks=False,
            )
        except FileExistsError:
            reject("transaction evidence output appeared before atomic creation")
        except OSError as error:
            reject(f"cannot publish transaction evidence manifest: {error}")
        published = True
        current = _stat_at_optional(parent_fd, name, "transaction evidence output")
        if current is None or not _same_file(temp_status, current):
            reject("transaction evidence output changed during atomic creation")
        _verify_directory_binding(
            context["rootPath"], context["rootStatus"], "transaction root"
        )
        _verify_directory_binding(
            context["parentPath"],
            context["parentStatus"],
            "transaction evidence output parent",
        )
        os.unlink(temp_name, dir_fd=parent_fd)
        temp_name = None
        current = _stat_at_optional(parent_fd, name, "transaction evidence output")
        if current is None or not _same_file(temp_status, current):
            reject("transaction evidence output changed after atomic creation")
        _validate_output_status(current, "transaction evidence output")
        os.fsync(parent_fd)
        current = _stat_at_optional(parent_fd, name, "transaction evidence output")
        if current is None or not _same_file(temp_status, current):
            reject("transaction evidence output changed before commit completed")
        _validate_output_status(current, "transaction evidence output")
        _verify_directory_binding(
            context["rootPath"], context["rootStatus"], "transaction root"
        )
        _verify_directory_binding(
            context["parentPath"],
            context["parentStatus"],
            "transaction evidence output parent",
        )
        committed = True
    except OSError as error:
        reject(f"cannot write transaction evidence manifest: {error}")
    finally:
        if descriptor is not None:
            os.close(descriptor)
        if published and not committed:
            current = _stat_at_optional(parent_fd, name, "transaction evidence output")
            if (
                temp_status is not None
                and current is not None
                and _same_file(temp_status, current)
            ):
                try:
                    os.unlink(name, dir_fd=parent_fd)
                except OSError:
                    pass
        if temp_name is not None:
            try:
                os.unlink(temp_name, dir_fd=parent_fd)
            except OSError:
                pass


def create_evidence_manifest(transaction_root, output_path):
    context = _open_evidence_context(transaction_root, output_path)
    try:
        value = _inventory_evidence(context)
        body = canonical_json(value)
        _commit_evidence_output(context, body)
        return value
    finally:
        _close_evidence_context(context)


def write_evidence_output(path, body):
    """Safely create, or byte-verify, an output without truncating it."""

    context = _open_evidence_context(os.path.dirname(os.path.abspath(path)), path)
    try:
        _commit_evidence_output(context, body)
    finally:
        _close_evidence_context(context)


def add_manifest_common_arguments(parser):
    parser.add_argument("--repository", required=True)
    parser.add_argument("--promotion-run-id", required=True)
    parser.add_argument("--promotion-run-attempt", required=True)
    parser.add_argument("--head-sha", required=True)
    parser.add_argument("--mode", required=True)
    parser.add_argument("--nonce", required=True)
    parser.add_argument("--qa-run-id", required=True)
    parser.add_argument("--qa-run-attempt", required=True)
    parser.add_argument("--qa-artifact-id", required=True)
    parser.add_argument("--qa-record", required=True)
    parser.add_argument("--expected-assets", required=True)
    parser.add_argument("--release-body", required=True)
    parser.add_argument("--artifact-api-digest", action="append", required=True)


def add_record_common_arguments(parser):
    parser.add_argument("--promotion-manifest", required=True)
    parser.add_argument("--issue-number", required=True)
    parser.add_argument("--dispatcher", required=True)
    parser.add_argument("--administrator", required=True)
    parser.add_argument("--gate-ready-at", required=True)
    parser.add_argument("--github-server-time", required=True)
    parser.add_argument("--settings-checked-at", required=True)
    parser.add_argument("--settings-response", required=True)
    parser.add_argument("--helper-commit", required=True)
    parser.add_argument(
        "--source-mode", choices=("run-bundle", "qa-reconstruction"), required=True
    )
    parser.add_argument("--source-run-id")
    parser.add_argument("--source-run-attempt")
    parser.add_argument("--source-artifact-id")
    parser.add_argument("--source-artifact-name")
    parser.add_argument("--source-api-digest")


def add_completion_arguments(parser):
    parser.add_argument("--authority-phase", required=True)
    parser.add_argument("--preflight-comment-id", required=True)
    parser.add_argument("--preflight-comment-url", required=True)
    parser.add_argument("--preflight-body-sha256", required=True)
    parser.add_argument("--tag-ref", required=True)
    parser.add_argument("--tag-sha", required=True)
    parser.add_argument("--release-id", required=True)
    parser.add_argument("--release-url", required=True)
    parser.add_argument("--release-tag", required=True)
    parser.add_argument("--release-name", required=True)
    parser.add_argument("--release-body-sha256", required=True)
    parser.add_argument("--release-draft", required=True)
    parser.add_argument("--release-immutable", required=True)
    parser.add_argument("--published-assets", required=True)
    parser.add_argument("--transaction-evidence-manifest-sha256", required=True)


def add_comment_verify_arguments(parser):
    parser.add_argument("--comments", required=True)
    parser.add_argument("--expected-author", required=True)
    parser.add_argument("--now", required=True)
    parser.add_argument("--expected-comment-id")
    parser.add_argument("--expected-body-sha256")
    parser.add_argument("--final-recheck", action="store_true")


def parser():
    root = argparse.ArgumentParser()
    commands = root.add_subparsers(dest="record", required=True)
    manifest = commands.add_parser("manifest")
    verbs = manifest.add_subparsers(dest="verb", required=True)
    create = verbs.add_parser("create")
    add_manifest_common_arguments(create)
    create.add_argument("--out", required=True)
    verify = verbs.add_parser("verify")
    add_manifest_common_arguments(verify)
    verify.add_argument("--manifest", required=True)

    preflight = commands.add_parser("preflight")
    preflight_verbs = preflight.add_subparsers(dest="verb", required=True)
    preflight_create = preflight_verbs.add_parser("create")
    add_record_common_arguments(preflight_create)
    preflight_create.add_argument("--out", required=True)
    preflight_verify = preflight_verbs.add_parser("verify")
    add_record_common_arguments(preflight_verify)
    add_comment_verify_arguments(preflight_verify)

    completion = commands.add_parser("completion")
    completion_verbs = completion.add_subparsers(dest="verb", required=True)
    completion_create = completion_verbs.add_parser("create")
    add_record_common_arguments(completion_create)
    add_completion_arguments(completion_create)
    completion_create.add_argument("--out", required=True)
    completion_verify = completion_verbs.add_parser("verify")
    add_record_common_arguments(completion_verify)
    add_completion_arguments(completion_verify)
    add_comment_verify_arguments(completion_verify)

    evidence = commands.add_parser("evidence")
    evidence_verbs = evidence.add_subparsers(dest="verb", required=True)
    evidence_create = evidence_verbs.add_parser("create")
    evidence_create.add_argument("--transaction-root", required=True)
    evidence_create.add_argument("--out", required=True)
    return root


def run_command(args):
    if args.record == "manifest":
        expected = build_manifest(args)
        if args.verb == "create":
            body = canonical_json(expected)
            write_bytes(args.out, body, "promotion manifest")
            print(f"wrote {args.out} ({len(body)} bytes)")
            return
        actual, body = read_json(args.manifest, "promotion manifest")
        validate_manifest(actual, body=body, expected=expected)
        print("promotion manifest verified")
        return
    if args.record == "preflight":
        expected = build_preflight(args)
        if args.verb == "create":
            validate_preflight(expected)
            body = encode_comment(PREFLIGHT_MARKER, expected)
            write_bytes(args.out, body, "preflight comment")
            print(f"wrote {args.out} ({len(body)} bytes)")
            return
        snapshot = verify_comment(
            args,
            PREFLIGHT_MARKER,
            "moor-release-preflight-v1",
            expected,
            validate_preflight,
            "preflight",
        )
        sys.stdout.buffer.write(canonical_json(snapshot))
        return
    if args.record == "completion":
        expected = build_completion(args)
        if args.verb == "create":
            validate_completion(expected)
            body = encode_comment(COMPLETION_MARKER, expected)
            write_bytes(args.out, body, "completion comment")
            print(f"wrote {args.out} ({len(body)} bytes)")
            return
        snapshot = verify_comment(
            args,
            COMPLETION_MARKER,
            "moor-release-completion-v1",
            expected,
            validate_completion,
            "completion",
        )
        sys.stdout.buffer.write(canonical_json(snapshot))
        return
    if args.record == "evidence":
        value = create_evidence_manifest(args.transaction_root, args.out)
        body = canonical_json(value)
        print(f"wrote {args.out} ({len(body)} bytes)")
        return
    reject("unsupported promotion record command")


def main():
    args = parser().parse_args()
    try:
        run_command(args)
    except Waiting as error:
        print(f"WAIT: {error}", file=sys.stderr)
        raise SystemExit(75)
    except Invalid as error:
        print(f"release-promotion-record: {error}", file=sys.stderr)
        raise SystemExit(1)


if __name__ == "__main__":
    main()
