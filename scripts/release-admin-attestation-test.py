#!/usr/bin/env python3
"""Acceptance, replay, and tamper tests for release-admin-attestation.py."""

import base64
import copy
import hashlib
import json
import os
import subprocess
import sys
import tempfile
from datetime import datetime, timedelta, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
TOOL = os.path.join(HERE, "release-admin-attestation.py")

REPOSITORY = "BrainyBlaze/moor"
HEAD_SHA = "a" * 40
QA_RUN_ID = "32016500124"
QA_RUN_ATTEMPT = "1"
QA_ARTIFACT_ID = "9283701943"
RUN_ID = "32020000001"
RUN_ATTEMPT = "1"
NONCE = "b" * 64
ISSUE_NUMBER = "26"
ACTOR = "levi770"
COMMENT_ID = 5315000001
GATE_READY_AT = "2026-08-17T12:30:00Z"
CHECKED_AT = "2026-08-17T12:30:02Z"
COMMENT_TIME = "2026-08-17T12:30:04Z"
NOW = "2026-08-17T12:30:05Z"
SETTINGS_RESPONSE = b'{"enabled":true,"enforced_by_owner":false}'


def canonical(value):
    return (json.dumps(value, indent=2, ensure_ascii=True) + "\n").encode("ascii")


def invoke(arguments):
    return subprocess.run(
        [sys.executable, TOOL, *arguments],
        capture_output=True,
        text=False,
    )


def create_command(response_path):
    return [
        "create",
        "--repository",
        REPOSITORY,
        "--head-sha",
        HEAD_SHA,
        "--qa-run-id",
        QA_RUN_ID,
        "--qa-run-attempt",
        QA_RUN_ATTEMPT,
        "--qa-artifact-id",
        QA_ARTIFACT_ID,
        "--run-id",
        RUN_ID,
        "--run-attempt",
        RUN_ATTEMPT,
        "--nonce",
        NONCE,
        "--gate-ready-at",
        GATE_READY_AT,
        "--checked-at",
        CHECKED_AT,
        "--response",
        response_path,
    ]


def comment(body, **overrides):
    value = {
        "id": COMMENT_ID,
        "html_url": (
            "https://github.com/BrainyBlaze/moor/issues/26"
            f"#issuecomment-{COMMENT_ID}"
        ),
        "issue_url": "https://api.github.com/repos/BrainyBlaze/moor/issues/26",
        "user": {"login": ACTOR},
        "created_at": COMMENT_TIME,
        "updated_at": COMMENT_TIME,
        "body": body.decode("ascii") if isinstance(body, bytes) else body,
    }
    value.update(overrides)
    return value


def verify_command(comments_path, **overrides):
    values = {
        "expected_repository": REPOSITORY,
        "expected_head_sha": HEAD_SHA,
        "expected_qa_run_id": QA_RUN_ID,
        "expected_qa_run_attempt": QA_RUN_ATTEMPT,
        "expected_qa_artifact_id": QA_ARTIFACT_ID,
        "expected_run_id": RUN_ID,
        "expected_run_attempt": RUN_ATTEMPT,
        "expected_nonce": NONCE,
        "expected_issue_number": ISSUE_NUMBER,
        "expected_actor": ACTOR,
        "gate_ready_at": GATE_READY_AT,
        "now": NOW,
    }
    values.update(overrides)
    arguments = ["verify", "--comments", comments_path]
    for name, value in values.items():
        option = f"--{name.replace('_', '-')}"
        if value is True:
            arguments.append(option)
        else:
            arguments.extend([option, str(value)])
    return arguments


def write_json(path, value):
    with open(path, "wb") as handle:
        handle.write(canonical(value))


def verify(root, comments, **overrides):
    path = os.path.join(root, "comments.json")
    write_json(path, comments)
    return invoke(verify_command(path, **overrides))


def reject(label, root, comments, **overrides):
    result = verify(root, comments, **overrides)
    assert result.returncode == 1, (
        f"{label}: expected invalid exit 1, got {result.returncode}: "
        f"{result.stderr.decode(errors='replace')}"
    )
    assert result.stderr.strip(), f"{label}: no diagnostic"


def shifted(timestamp, seconds):
    parsed = datetime.strptime(timestamp, "%Y-%m-%dT%H:%M:%SZ").replace(
        tzinfo=timezone.utc
    )
    return (parsed + timedelta(seconds=seconds)).strftime("%Y-%m-%dT%H:%M:%SZ")


def main():
    assert len(SETTINGS_RESPONSE) == 42
    with tempfile.TemporaryDirectory(prefix="release-admin-attestation-") as root:
        response_path = os.path.join(root, "immutable-settings-response.json")
        with open(response_path, "wb") as handle:
            handle.write(SETTINGS_RESPONSE)

        created = invoke(create_command(response_path))
        assert created.returncode == 0, created.stderr.decode(errors="replace")
        attestation = json.loads(created.stdout)
        assert created.stdout == canonical(attestation)
        assert list(attestation) == [
            "schemaVersion",
            "kind",
            "repository",
            "promotion",
            "immutableReleaseSettings",
        ]
        assert attestation["promotion"] == {
            "workflowRunId": RUN_ID,
            "workflowRunAttempt": 1,
            "headSha": HEAD_SHA,
            "qaRunId": QA_RUN_ID,
            "qaRunAttempt": 1,
            "qaArtifactId": QA_ARTIFACT_ID,
            "nonce": NONCE,
            "gateReadyAt": GATE_READY_AT,
        }
        assert attestation["immutableReleaseSettings"] == {
            "checkedAt": CHECKED_AT,
            "responseBase64": base64.b64encode(SETTINGS_RESPONSE).decode("ascii"),
            "responseSha256": hashlib.sha256(SETTINGS_RESPONSE).hexdigest(),
        }

        body = created.stdout
        valid_comment = comment(body)
        accepted = verify(root, [{"body": "unrelated"}, valid_comment])
        assert accepted.returncode == 0, accepted.stderr.decode(errors="replace")
        snapshot = json.loads(accepted.stdout)
        assert accepted.stdout == canonical(snapshot)
        assert snapshot["commentId"] == str(COMMENT_ID)
        assert snapshot["commentUrl"] == valid_comment["html_url"]
        assert snapshot["author"] == ACTOR
        assert snapshot["bodySha256"] == hashlib.sha256(body).hexdigest()
        assert snapshot["attestation"] == attestation

        for response in (
            b'{"enabled":true,"enforced_by_owner":true}',
            b'{"enforced_by_owner":false,"enabled":true}',
        ):
            alternate_response_path = os.path.join(root, "alternate-response.json")
            with open(alternate_response_path, "wb") as handle:
                handle.write(response)
            alternate_created = invoke(create_command(alternate_response_path))
            assert alternate_created.returncode == 0, alternate_created.stderr.decode(
                errors="replace"
            )
            alternate_accepted = verify(root, [comment(alternate_created.stdout)])
            assert alternate_accepted.returncode == 0, alternate_accepted.stderr.decode(
                errors="replace"
            )

        unicode_comments_path = os.path.join(root, "unicode-comments.json")
        with open(unicode_comments_path, "wb") as handle:
            handle.write(
                json.dumps(
                    [{"body": "unrelated ☕"}, valid_comment],
                    ensure_ascii=False,
                ).encode("utf-8")
            )
        unicode_accepted = invoke(verify_command(unicode_comments_path))
        assert unicode_accepted.returncode == 0, unicode_accepted.stderr.decode(
            errors="replace"
        )
        third_party_noise = dict(
            valid_comment,
            id=COMMENT_ID + 1,
            user={"login": "other-admin"},
        )
        noise_accepted = verify(root, [third_party_noise, valid_comment])
        assert noise_accepted.returncode == 0, noise_accepted.stderr.decode(
            errors="replace"
        )

        missing = verify(root, [])
        assert missing.returncode == 75, missing.stderr.decode(errors="replace")
        assert b"no matching attestation comment" in missing.stderr
        reject("malformed expected nonce", root, [valid_comment], expected_nonce="bad")

        reject(
            "duplicate nonce",
            root,
            [valid_comment, dict(valid_comment, id=COMMENT_ID + 1)],
        )
        reject(
            "edited comment",
            root,
            [dict(valid_comment, updated_at=shifted(COMMENT_TIME, 1))],
        )
        wrong_author = verify(
            root, [dict(valid_comment, user={"login": "other-admin"})]
        )
        assert wrong_author.returncode == 75, wrong_author.stderr.decode(
            errors="replace"
        )
        reject(
            "wrong issue URL",
            root,
            [
                dict(
                    valid_comment,
                    issue_url="https://api.github.com/repos/BrainyBlaze/moor/issues/27",
                )
            ],
        )
        reject(
            "wrong comment URL",
            root,
            [
                dict(
                    valid_comment,
                    html_url=valid_comment["html_url"].replace("26", "27", 1),
                )
            ],
        )

        for label, path, replacement in (
            ("repository", ("repository",), "BrainyBlaze/desk"),
            ("head", ("promotion", "headSha"), "c" * 40),
            ("QA run", ("promotion", "qaRunId"), "1"),
            ("QA attempt", ("promotion", "qaRunAttempt"), 2),
            ("QA artifact", ("promotion", "qaArtifactId"), "1"),
            ("promotion run", ("promotion", "workflowRunId"), "1"),
            ("promotion attempt", ("promotion", "workflowRunAttempt"), 2),
            ("gate boundary", ("promotion", "gateReadyAt"), shifted(GATE_READY_AT, 1)),
        ):
            mutated = copy.deepcopy(attestation)
            target = mutated
            for key in path[:-1]:
                target = target[key]
            target[path[-1]] = replacement
            reject(label, root, [comment(canonical(mutated))])

        extra = copy.deepcopy(attestation)
        extra["unexpected"] = True
        reject("extra key", root, [comment(canonical(extra))])
        boolean_schema = copy.deepcopy(attestation)
        boolean_schema["schemaVersion"] = True
        reject("boolean schema version", root, [comment(canonical(boolean_schema))])
        missing_key = copy.deepcopy(attestation)
        del missing_key["immutableReleaseSettings"]["responseSha256"]
        reject("missing key", root, [comment(canonical(missing_key))])
        reject("noncanonical JSON", root, [comment(json.dumps(attestation))])

        duplicate_key_body = body.replace(
            b'  "kind": "moor-release-immutable-settings-v1",\n',
            b'  "kind": "moor-release-immutable-settings-v1",\n'
            b'  "kind": "moor-release-immutable-settings-v1",\n',
        )
        reject("duplicate JSON key", root, [comment(duplicate_key_body)])
        masked_nonce_body = body.replace(
            f'    "nonce": "{NONCE}",\n'.encode("ascii"),
            f'    "nonce": "{NONCE}",\n'.encode("ascii")
            + f'    "nonce": "{"c" * 64}",\n'.encode("ascii"),
        )
        reject(
            "duplicate key cannot mask the expected nonce",
            root,
            [comment(masked_nonce_body)],
        )

        for label, response in (
            (
                "disabled response",
                b'{"enabled":false,"enforced_by_owner":false}\n',
            ),
            ("missing owner enforcement", b'{"enabled":true}\n'),
            (
                "invalid owner enforcement",
                b'{"enabled":true,"enforced_by_owner":"false"}\n',
            ),
            (
                "extra response key",
                b'{"enabled":true,"enforced_by_owner":false,"other":1}\n',
            ),
            ("malformed response", b'{"enabled":'),
        ):
            mutated = copy.deepcopy(attestation)
            mutated["immutableReleaseSettings"]["responseBase64"] = base64.b64encode(
                response
            ).decode("ascii")
            mutated["immutableReleaseSettings"]["responseSha256"] = hashlib.sha256(
                response
            ).hexdigest()
            reject(label, root, [comment(canonical(mutated))])

        wrong_hash = copy.deepcopy(attestation)
        wrong_hash["immutableReleaseSettings"]["responseSha256"] = "0" * 64
        reject("response digest", root, [comment(canonical(wrong_hash))])
        noncanonical_base64 = copy.deepcopy(attestation)
        noncanonical_base64["immutableReleaseSettings"]["responseBase64"] += "\n"
        reject("response base64", root, [comment(canonical(noncanonical_base64))])

        checked_before_gate = copy.deepcopy(attestation)
        checked_before_gate["immutableReleaseSettings"]["checkedAt"] = shifted(
            GATE_READY_AT, -6
        )
        reject(
            "settings read before gate", root, [comment(canonical(checked_before_gate))]
        )
        reject(
            "comment before gate",
            root,
            [
                dict(
                    valid_comment,
                    created_at=shifted(GATE_READY_AT, -6),
                    updated_at=shifted(GATE_READY_AT, -6),
                )
            ],
        )
        reject("stale comment", root, [valid_comment], now=shifted(COMMENT_TIME, 901))
        reject(
            "stale settings read",
            root,
            [
                dict(
                    valid_comment,
                    created_at=shifted(COMMENT_TIME, 901),
                    updated_at=shifted(COMMENT_TIME, 901),
                )
            ],
            now=shifted(COMMENT_TIME, 902),
        )
        reject("future comment", root, [valid_comment], now=shifted(COMMENT_TIME, -6))

        body_sha = hashlib.sha256(body).hexdigest()
        reject(
            "postpublish recheck requires accepted identity",
            root,
            [valid_comment],
            postpublish_recheck=True,
        )
        final = verify(
            root,
            valid_comment,
            expected_comment_id=str(COMMENT_ID),
            expected_body_sha256=body_sha,
            now=shifted(COMMENT_TIME, 1800),
            postpublish_recheck=True,
        )
        assert final.returncode == 0, final.stderr.decode(errors="replace")
        reject(
            "postpublish comment identity",
            root,
            [valid_comment],
            expected_comment_id=str(COMMENT_ID + 1),
            expected_body_sha256=body_sha,
        )
        reject(
            "postpublish body digest",
            root,
            [valid_comment],
            expected_comment_id=str(COMMENT_ID),
            expected_body_sha256="0" * 64,
        )

    print("release admin attestation tests: create/accept plus 34 refusal cases passed")


if __name__ == "__main__":
    main()
