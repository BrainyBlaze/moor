#!/usr/bin/env python3
"""Create and verify one run-bound immutable-release settings attestation."""

import argparse
import base64
import binascii
import hashlib
import json
import re
import sys
from datetime import datetime, timezone

KIND = "moor-release-immutable-settings-v1"
CLOCK_SKEW_SECONDS = 5
MAX_COMMENT_AGE_SECONDS = 15 * 60

DECIMAL = re.compile(r"^[1-9][0-9]*$")
HEX40 = re.compile(r"^[0-9a-f]{40}$")
HEX64 = re.compile(r"^[0-9a-f]{64}$")
NONCE = re.compile(r"^[0-9a-f]{64}$")
REPOSITORY = re.compile(r"^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$")
TIMESTAMP = re.compile(r"^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$")
ACTOR = re.compile(r"^[A-Za-z0-9](?:[A-Za-z0-9-]{0,37}[A-Za-z0-9])?$")


class Invalid(Exception):
    pass


class Waiting(Exception):
    pass


def reject(message):
    raise Invalid(message)


def exact_keys(value, keys, what):
    if not isinstance(value, dict) or list(value) != keys:
        actual = list(value) if isinstance(value, dict) else type(value).__name__
        reject(f"{what} keys are {actual}, expected {keys}")


def duplicate_guard(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            reject(f"duplicate JSON key {key!r}")
        result[key] = value
    return result


def canonical(value):
    return (json.dumps(value, indent=2, ensure_ascii=True) + "\n").encode("ascii")


def parse_json_bytes(body, what):
    try:
        text = body.decode("ascii")
        return json.loads(text, object_pairs_hook=duplicate_guard)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        reject(f"invalid {what}: {error}")


def read_bytes(path, what):
    try:
        with open(path, "rb") as handle:
            return handle.read()
    except OSError as error:
        reject(f"cannot read {what}: {error}")


def read_json(path, what):
    try:
        text = read_bytes(path, what).decode("utf-8")
        return json.loads(text, object_pairs_hook=duplicate_guard)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        reject(f"invalid {what}: {error}")


def decimal(value, what):
    if not isinstance(value, str) or not DECIMAL.fullmatch(value):
        reject(f"{what} is not a nonzero decimal string")
    if int(value) > 9007199254740991:
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


def sha64(value, what):
    if not isinstance(value, str) or not HEX64.fullmatch(value):
        reject(f"{what} is not lowercase SHA-256")
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


def validate_settings_response(body):
    if not body or len(body) > 4096:
        reject("immutable settings response has an invalid byte length")
    value = parse_json_bytes(body, "immutable settings response")
    expected_keys = {"enabled", "enforced_by_owner"}
    if not isinstance(value, dict) or set(value) != expected_keys:
        actual = list(value) if isinstance(value, dict) else type(value).__name__
        reject(
            f"immutable settings response keys are {actual}, "
            f"expected exactly {sorted(expected_keys)}"
        )
    if value["enabled"] is not True:
        reject("immutable releases were not enabled")
    if not isinstance(value["enforced_by_owner"], bool):
        reject("immutable settings owner enforcement is not boolean")


def build_attestation(args):
    response = read_bytes(args.response, "immutable settings response")
    validate_settings_response(response)
    repository(args.repository, "repository")
    sha40(args.head_sha, "head SHA")
    decimal(args.qa_run_id, "QA run ID")
    decimal(args.qa_artifact_id, "QA artifact ID")
    decimal(args.run_id, "promotion run ID")
    nonce(args.nonce, "attestation nonce")
    if args.qa_run_attempt != "1" or args.run_attempt != "1":
        reject("QA and promotion attempts must both be 1")
    gate_ready_at = timestamp(args.gate_ready_at, "gate-ready time")
    checked_at = timestamp(args.checked_at, "settings checked time")
    if (checked_at - gate_ready_at).total_seconds() < -CLOCK_SKEW_SECONDS:
        reject("immutable settings were read before the attestation gate was ready")
    return {
        "schemaVersion": 1,
        "kind": KIND,
        "repository": args.repository,
        "promotion": {
            "workflowRunId": args.run_id,
            "workflowRunAttempt": 1,
            "headSha": args.head_sha,
            "qaRunId": args.qa_run_id,
            "qaRunAttempt": 1,
            "qaArtifactId": args.qa_artifact_id,
            "nonce": args.nonce,
            "gateReadyAt": args.gate_ready_at,
        },
        "immutableReleaseSettings": {
            "checkedAt": args.checked_at,
            "responseBase64": base64.b64encode(response).decode("ascii"),
            "responseSha256": hashlib.sha256(response).hexdigest(),
        },
    }


def loose_envelope(body):
    try:
        return json.loads(body)
    except (TypeError, json.JSONDecodeError):
        return None


def candidate_comments(comments, expected_nonce, expected_actor):
    candidates = []
    for comment in comments:
        if not isinstance(comment, dict) or not isinstance(comment.get("body"), str):
            continue
        user = comment.get("user")
        if not isinstance(user, dict) or user.get("login") != expected_actor:
            continue
        body = comment["body"]
        envelope = loose_envelope(body)
        structurally_matches = False
        if isinstance(envelope, dict):
            promotion = envelope.get("promotion")
            structurally_matches = (
                envelope.get("kind") == KIND
                and isinstance(promotion, dict)
                and promotion.get("nonce") == expected_nonce
            )
        if structurally_matches or (KIND in body and expected_nonce in body):
            candidates.append(comment)
    return candidates


def validate_attestation(value, body, args):
    exact_keys(
        value,
        [
            "schemaVersion",
            "kind",
            "repository",
            "promotion",
            "immutableReleaseSettings",
        ],
        "attestation",
    )
    attempt(value["schemaVersion"], "attestation schema version")
    if value["kind"] != KIND:
        reject("attestation fixed identity differs")
    if (
        repository(value["repository"], "attestation repository")
        != args.expected_repository
    ):
        reject("attestation cites another repository")
    promotion = value["promotion"]
    exact_keys(
        promotion,
        [
            "workflowRunId",
            "workflowRunAttempt",
            "headSha",
            "qaRunId",
            "qaRunAttempt",
            "qaArtifactId",
            "nonce",
            "gateReadyAt",
        ],
        "attestation promotion",
    )
    expected = (
        (
            decimal(promotion["workflowRunId"], "attestation promotion run ID"),
            args.expected_run_id,
            "promotion run",
        ),
        (
            sha40(promotion["headSha"], "attestation head SHA"),
            args.expected_head_sha,
            "head SHA",
        ),
        (
            decimal(promotion["qaRunId"], "attestation QA run ID"),
            args.expected_qa_run_id,
            "QA run",
        ),
        (
            decimal(promotion["qaArtifactId"], "attestation QA artifact ID"),
            args.expected_qa_artifact_id,
            "QA artifact",
        ),
        (nonce(promotion["nonce"], "attestation nonce"), args.expected_nonce, "nonce"),
        (promotion["gateReadyAt"], args.gate_ready_at, "gate-ready time"),
    )
    attempt(promotion["workflowRunAttempt"], "attestation promotion attempt")
    attempt(promotion["qaRunAttempt"], "attestation QA attempt")
    if args.expected_run_attempt != "1" or args.expected_qa_run_attempt != "1":
        reject("expected QA and promotion attempts must both be 1")
    for actual, wanted, what in expected:
        if actual != wanted:
            reject(f"attestation cites another {what}")
    timestamp(promotion["gateReadyAt"], "attestation gate-ready time")

    settings = value["immutableReleaseSettings"]
    exact_keys(
        settings,
        ["checkedAt", "responseBase64", "responseSha256"],
        "attestation immutable settings",
    )
    checked_at = timestamp(settings["checkedAt"], "attestation settings checked time")
    if not isinstance(settings["responseBase64"], str):
        reject("attestation responseBase64 is not a string")
    try:
        response = base64.b64decode(settings["responseBase64"], validate=True)
    except (binascii.Error, ValueError) as error:
        reject(f"attestation responseBase64 is invalid: {error}")
    if base64.b64encode(response).decode("ascii") != settings["responseBase64"]:
        reject("attestation responseBase64 is not canonical")
    if (
        sha64(settings["responseSha256"], "attestation response SHA-256")
        != hashlib.sha256(response).hexdigest()
    ):
        reject("attestation response SHA-256 differs from the exact response bytes")
    validate_settings_response(response)
    if body != canonical(value):
        reject("attestation body is not canonical two-space JSON with one LF")
    return checked_at


def validate_verify_expectations(args):
    repository(args.expected_repository, "expected repository")
    sha40(args.expected_head_sha, "expected head SHA")
    decimal(args.expected_qa_run_id, "expected QA run ID")
    decimal(args.expected_qa_artifact_id, "expected QA artifact ID")
    decimal(args.expected_run_id, "expected promotion run ID")
    decimal(args.expected_issue_number, "expected issue number")
    nonce(args.expected_nonce, "expected attestation nonce")
    actor(args.expected_actor, "expected actor")
    timestamp(args.gate_ready_at, "expected gate-ready time")
    timestamp(args.now, "verification time")
    if args.expected_qa_run_attempt != "1" or args.expected_run_attempt != "1":
        reject("expected QA and promotion attempts must both be 1")
    if args.expected_comment_id is not None:
        decimal(args.expected_comment_id, "expected comment ID")
    if args.expected_body_sha256 is not None:
        sha64(args.expected_body_sha256, "expected comment body SHA-256")
    if args.postpublish_recheck and (
        args.expected_comment_id is None or args.expected_body_sha256 is None
    ):
        reject(
            "postpublication recheck requires the accepted comment ID and body SHA-256"
        )


def verify_comment(args):
    validate_verify_expectations(args)
    comments = read_json(args.comments, "attestation comments")
    if isinstance(comments, dict):
        comments = [comments]
    if not isinstance(comments, list):
        reject("attestation comments are not an array or object")
    matches = candidate_comments(comments, args.expected_nonce, args.expected_actor)
    if not matches:
        raise Waiting("no matching attestation comment yet")
    if len(matches) != 1:
        reject(
            f"expected exactly one matching attestation comment, found {len(matches)}"
        )
    comment = matches[0]
    try:
        body = comment["body"].encode("ascii")
    except (KeyError, AttributeError, UnicodeEncodeError) as error:
        reject(f"attestation comment body is invalid: {error}")
    value = parse_json_bytes(body, "attestation comment body")
    checked_at = validate_attestation(value, body, args)

    comment_id = comment.get("id")
    if (
        not isinstance(comment_id, int)
        or isinstance(comment_id, bool)
        or comment_id <= 0
    ):
        reject("attestation comment ID is not a positive integer")
    comment_id_text = str(comment_id)
    if (
        args.expected_comment_id is not None
        and comment_id_text != args.expected_comment_id
    ):
        reject("attestation comment ID changed")
    body_sha256 = hashlib.sha256(body).hexdigest()
    if (
        args.expected_body_sha256 is not None
        and body_sha256 != args.expected_body_sha256
    ):
        reject("attestation comment body SHA-256 changed")

    actor(args.expected_actor, "expected actor")
    user = comment.get("user")
    if not isinstance(user, dict) or user.get("login") != args.expected_actor:
        reject("attestation comment author differs from the promotion actor")
    expected_issue_url = (
        f"https://api.github.com/repos/{args.expected_repository}/issues/"
        f"{args.expected_issue_number}"
    )
    expected_comment_url = (
        f"https://github.com/{args.expected_repository}/issues/{args.expected_issue_number}"
        f"#issuecomment-{comment_id_text}"
    )
    if comment.get("issue_url") != expected_issue_url:
        reject("attestation comment belongs to another issue")
    if comment.get("html_url") != expected_comment_url:
        reject("attestation comment URL is not canonical for its ID")
    if comment.get("created_at") != comment.get("updated_at"):
        reject("attestation comment was edited")

    gate_ready_at = timestamp(args.gate_ready_at, "expected gate-ready time")
    created_at = timestamp(
        comment.get("created_at"), "attestation comment creation time"
    )
    now = timestamp(args.now, "verification time")
    if (checked_at - gate_ready_at).total_seconds() < -CLOCK_SKEW_SECONDS:
        reject("immutable settings were read before the attestation gate was ready")
    if (created_at - gate_ready_at).total_seconds() < -CLOCK_SKEW_SECONDS:
        reject("attestation comment was created before the gate was ready")
    if (checked_at - created_at).total_seconds() > CLOCK_SKEW_SECONDS:
        reject("immutable settings check is later than the attestation comment")
    if (checked_at - now).total_seconds() > CLOCK_SKEW_SECONDS:
        reject("immutable settings check is in the future")
    if (created_at - now).total_seconds() > CLOCK_SKEW_SECONDS:
        reject("attestation comment is in the future")
    if (
        not args.postpublish_recheck
        and (now - created_at).total_seconds() > MAX_COMMENT_AGE_SECONDS
    ):
        reject("attestation comment is stale")
    if (
        not args.postpublish_recheck
        and (now - checked_at).total_seconds() > MAX_COMMENT_AGE_SECONDS
    ):
        reject("immutable settings check is stale")

    return {
        "schemaVersion": 1,
        "commentId": comment_id_text,
        "commentUrl": expected_comment_url,
        "author": args.expected_actor,
        "createdAt": comment["created_at"],
        "updatedAt": comment["updated_at"],
        "bodySha256": body_sha256,
        "attestation": value,
    }


def add_create_arguments(parser):
    parser.add_argument("--repository", required=True)
    parser.add_argument("--head-sha", required=True)
    parser.add_argument("--qa-run-id", required=True)
    parser.add_argument("--qa-run-attempt", required=True)
    parser.add_argument("--qa-artifact-id", required=True)
    parser.add_argument("--run-id", required=True)
    parser.add_argument("--run-attempt", required=True)
    parser.add_argument("--nonce", required=True)
    parser.add_argument("--gate-ready-at", required=True)
    parser.add_argument("--checked-at", required=True)
    parser.add_argument("--response", required=True)


def add_verify_arguments(parser):
    parser.add_argument("--comments", required=True)
    parser.add_argument("--expected-repository", required=True)
    parser.add_argument("--expected-head-sha", required=True)
    parser.add_argument("--expected-qa-run-id", required=True)
    parser.add_argument("--expected-qa-run-attempt", required=True)
    parser.add_argument("--expected-qa-artifact-id", required=True)
    parser.add_argument("--expected-run-id", required=True)
    parser.add_argument("--expected-run-attempt", required=True)
    parser.add_argument("--expected-nonce", required=True)
    parser.add_argument("--expected-issue-number", required=True)
    parser.add_argument("--expected-actor", required=True)
    parser.add_argument("--gate-ready-at", required=True)
    parser.add_argument("--now", required=True)
    parser.add_argument("--expected-comment-id")
    parser.add_argument("--expected-body-sha256")
    parser.add_argument("--postpublish-recheck", action="store_true")


def parse_args():
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="command", required=True)
    create = subparsers.add_parser("create")
    add_create_arguments(create)
    verify = subparsers.add_parser("verify")
    add_verify_arguments(verify)
    return parser.parse_args()


def main():
    args = parse_args()
    try:
        if args.command == "create":
            result = build_attestation(args)
        else:
            result = verify_comment(args)
        sys.stdout.buffer.write(canonical(result))
    except Waiting as error:
        print(f"WAIT: {error}", file=sys.stderr)
        raise SystemExit(75)
    except Invalid as error:
        print(f"ERROR: {error}", file=sys.stderr)
        raise SystemExit(1)


if __name__ == "__main__":
    main()
