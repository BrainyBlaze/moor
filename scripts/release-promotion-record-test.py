#!/usr/bin/env python3
"""Deterministic acceptance and refusal tests for promotion records."""

import copy
import base64
import hashlib
import importlib.util
import json
import os
import subprocess
import sys
import tempfile
from datetime import datetime, timedelta, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
TOOL = os.path.join(HERE, "release_promotion_record.py")

PREFLIGHT_MARKER = b"<!-- moor-release-preflight-v1 -->\n"
COMPLETION_MARKER = b"<!-- moor-release-completion-v1 -->\n"

REPOSITORY = "BrainyBlaze/moor"
VERSION = "v0.1.0"
CANDIDATE_SHA = "a" * 40
HEAD_SHA = "b" * 40
HELPER_SHA = "c" * 40
CANDIDATE_RUN_ID = "32003467728"
CANDIDATE_QA_RUN_ID = "32015916481"
QA_RUN_ID = "32016500124"
QA_ARTIFACT_ID = "9283701943"
PROMOTION_RUN_ID = "32030000000"
NONCE = "d" * 64
ISSUE_NUMBER = "26"
DISPATCHER = "levi770"
COMMENT_ID = 5315000001
PREFLIGHT_GATE_READY_AT = "2026-08-17T12:30:00Z"
PREFLIGHT_SERVER_TIME = "2026-08-17T12:30:01Z"
PREFLIGHT_CHECKED_AT = "2026-08-17T12:30:02Z"
PREFLIGHT_COMMENT_TIME = "2026-08-17T12:30:04Z"
PREFLIGHT_NOW = "2026-08-17T12:30:05Z"
COMPLETION_SERVER_TIME = "2026-08-17T12:40:01Z"
COMPLETION_CHECKED_AT = "2026-08-17T12:40:02Z"
COMPLETION_COMMENT_TIME = "2026-08-17T12:40:04Z"
COMPLETION_NOW = "2026-08-17T12:40:05Z"
SETTINGS_RESPONSE = b'{"enabled":true,"enforced_by_owner":false}'
TARGETS = [
    ("x86_64-unknown-linux-musl", "moor-0.1.0-linux-x64"),
    ("aarch64-unknown-linux-musl", "moor-0.1.0-linux-arm64"),
    ("x86_64-apple-darwin", "moor-0.1.0-macos-x64"),
    ("aarch64-apple-darwin", "moor-0.1.0-macos-arm64"),
]


def canonical(value):
    return json.dumps(
        value, sort_keys=True, separators=(",", ":"), ensure_ascii=True
    ).encode("ascii") + b"\n"


def legacy_canonical(value):
    return (json.dumps(value, indent=2, ensure_ascii=True) + "\n").encode("ascii")


def assert_marker(body, marker):
    assert body.startswith(marker)
    assert body == marker + canonical(json.loads(body[len(marker):]))


def load_tool():
    spec = importlib.util.spec_from_file_location("release_promotion_record", TOOL)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def invoke(arguments):
    return subprocess.run(
        [sys.executable, TOOL, *arguments], capture_output=True, text=False
    )


def expect_invalid(label, operation):
    try:
        operation()
    except Exception as error:
        assert error.__class__.__name__ == "Invalid", (
            f"{label}: expected Invalid, got {type(error).__name__}: {error}"
        )
    else:
        raise AssertionError(f"{label}: invalid value was accepted")


def write_bytes(path, body):
    with open(path, "wb") as handle:
        handle.write(body)


def shifted(value, seconds):
    parsed = datetime.strptime(value, "%Y-%m-%dT%H:%M:%SZ").replace(
        tzinfo=timezone.utc
    )
    return (parsed + timedelta(seconds=seconds)).strftime("%Y-%m-%dT%H:%M:%SZ")


def make_qa_record():
    targets = {}
    for index, (target, name) in enumerate(TARGETS):
        targets[target] = {
            "asset": name,
            "size": 1000 + index,
            "sha256": hashlib.sha256(name.encode("ascii")).hexdigest(),
            "artifactId": str(9279395000 + index),
            "artifactName": f"moor-candidate-{target}",
            "manualQa": {
                "verdict": "passed",
                "evidence": (
                    "https://github.com/BrainyBlaze/moor/actions/runs/"
                    + CANDIDATE_QA_RUN_ID
                ),
            },
        }
    return {
        "schemaVersion": 1,
        "repository": "https://github.com/BrainyBlaze/moor",
        "version": VERSION,
        "commit": CANDIDATE_SHA,
        "candidate": {
            "workflowRunId": CANDIDATE_RUN_ID,
            "workflowRunAttempt": 1,
            "metadataArtifactId": "9279394300",
            "metadataArtifactName": "moor-release-candidate-v1",
            "candidateRecordArtifactId": "9279395137",
            "candidateRecordArtifactName": "moor-release-candidate-record",
            "manifestSha256": "1" * 64,
            "sha256sumsSha256": "2" * 64,
        },
        "candidateQa": {
            "workflowRunId": CANDIDATE_QA_RUN_ID,
            "workflowRunAttempt": 1,
            "evidenceArtifactId": "9283559446",
            "evidenceArtifactName": "moor-release-candidate-qa-evidence",
            "deskCommit": "e" * 40,
        },
        "qaRun": {"workflowRunId": QA_RUN_ID, "workflowRunAttempt": 1},
        "coverage": {"requiredClosure": "full-matrix"},
        "targets": targets,
        "manualQa": {
            "verdict": "passed",
            "checklist": [],
            "approvedBy": "levi770",
            "approvedAt": "2026-08-17T09:30:00Z",
            "evidence": {},
            "confirmation": "fixture",
        },
    }


def candidate_artifacts(qa):
    candidate = qa["candidate"]
    result = [
        (candidate["metadataArtifactId"], candidate["metadataArtifactName"]),
        (
            candidate["candidateRecordArtifactId"],
            candidate["candidateRecordArtifactName"],
        ),
    ]
    result += [
        (entry["artifactId"], entry["artifactName"])
        for entry in qa["targets"].values()
    ]
    return result


def make_expected_assets(qa):
    assets = [
        {"name": entry["asset"], "size": entry["size"], "sha256": entry["sha256"]}
        for entry in qa["targets"].values()
    ]
    assets += [
        {
            "name": "moor-release-manifest-v1.json",
            "size": 2000,
            "sha256": qa["candidate"]["manifestSha256"],
        },
        {
            "name": "SHA256SUMS",
            "size": 400,
            "sha256": qa["candidate"]["sha256sumsSha256"],
        },
    ]
    return sorted(assets, key=lambda item: item["name"].encode("ascii"))


def release_body(qa):
    return (
        f"Source-Commit: {qa['commit']}\n"
        f"Candidate-Run: {qa['candidate']['workflowRunId']}/1\n"
        f"Promotion-Transaction: {QA_RUN_ID}/1/{QA_ARTIFACT_ID}"
    ).encode("ascii")


def manifest_paths(root, qa=None, assets=None, body=None):
    qa = make_qa_record() if qa is None else qa
    assets = make_expected_assets(qa) if assets is None else assets
    body = release_body(qa) if body is None else body
    paths = {
        "qa": os.path.join(root, "moor-release-qa-v1.json"),
        "assets": os.path.join(root, "expected-assets.json"),
        "body": os.path.join(root, "release-body.txt"),
        "manifest": os.path.join(root, "promotion-manifest.json"),
    }
    write_bytes(paths["qa"], legacy_canonical(qa))
    write_bytes(paths["assets"], canonical(assets))
    write_bytes(paths["body"], body)
    return qa, assets, paths


def manifest_common(paths, qa, **overrides):
    values = {
        "repository": REPOSITORY,
        "promotion_run_id": PROMOTION_RUN_ID,
        "promotion_run_attempt": "1",
        "head_sha": HEAD_SHA,
        "mode": "promote",
        "nonce": NONCE,
        "qa_run_id": QA_RUN_ID,
        "qa_run_attempt": "1",
        "qa_artifact_id": QA_ARTIFACT_ID,
        "qa_record": paths["qa"],
        "expected_assets": paths["assets"],
        "release_body": paths["body"],
    }
    values.update(overrides)
    arguments = []
    for name, value in values.items():
        arguments.extend([f"--{name.replace('_', '-')}", str(value)])
    ids = [artifact_id for artifact_id, _ in candidate_artifacts(qa)]
    ids += [qa["candidateQa"]["evidenceArtifactId"], QA_ARTIFACT_ID]
    for index, artifact_id in enumerate(ids):
        arguments.extend(
            [
                "--artifact-api-digest",
                f"{artifact_id}=sha256:{index + 1:064x}",
            ]
        )
    return arguments


def run_manifest(root, verb="create", qa=None, assets=None, body=None, **overrides):
    qa, assets, paths = manifest_paths(root, qa=qa, assets=assets, body=body)
    arguments = ["manifest", verb] + manifest_common(paths, qa, **overrides)
    if verb == "create":
        arguments += ["--out", paths["manifest"]]
    else:
        arguments += ["--manifest", paths["manifest"]]
    return invoke(arguments), qa, assets, paths


def manifest_reject(label, mutate=None, verb="create", **overrides):
    with tempfile.TemporaryDirectory(prefix="release-promotion-manifest-reject-") as root:
        qa = make_qa_record()
        assets = make_expected_assets(qa)
        body = release_body(qa)
        if mutate is not None:
            qa, assets, body = mutate(qa, assets, body)
        if verb == "verify":
            created, _, _, paths = run_manifest(root, qa=qa, assets=assets, body=body)
            assert created.returncode == 0, created.stderr.decode(errors="replace")
            if mutate is not None:
                mutate(paths, None, None)
            arguments = ["manifest", "verify"] + manifest_common(paths, qa, **overrides)
            arguments += ["--manifest", paths["manifest"]]
            result = invoke(arguments)
        else:
            result, _, _, _ = run_manifest(
                root, qa=qa, assets=assets, body=body, **overrides
            )
        assert result.returncode == 1, (
            f"{label}: expected exit 1, got {result.returncode}: "
            f"{result.stderr.decode(errors='replace')}"
        )
        assert result.stderr.strip(), f"{label}: no diagnostic"


def test_primitives(tool):
    assert tool.CLOCK_SKEW_SECONDS == 5
    assert tool.COMMENT_FRESHNESS_SECONDS == 15 * 60
    assert tool.PREFLIGHT_WAIT_SECONDS == 15 * 60
    assert tool.COMPLETION_WAIT_SECONDS == 60 * 60
    assert tool.POLL_INTERVAL_SECONDS == 5
    assert tool.AMBIGUOUS_OBSERVATION_SECONDS == 30
    with open(TOOL, encoding="ascii") as handle:
        assert ".total_seconds(" not in handle.read(), (
            "timestamp validation must use exact timedelta comparisons"
        )

    value = {"z": 1, "a": {"nonce": "b" * 64}}
    assert tool.canonical_json(value) == canonical(value)
    for marker in (PREFLIGHT_MARKER, COMPLETION_MARKER):
        body = tool.encode_comment(marker, value)
        assert_marker(body, marker)
        assert tool.decode_comment(marker, body, "record") == value

    valid = b'{"enabled":true,"enforced_by_owner":false}'
    assert tool.validate_settings_response(valid) == {
        "enabled": True,
        "enforced_by_owner": False,
    }
    assert tool.validate_settings_response(
        b'{"enforced_by_owner":true,"enabled":true}'
    ) == {"enforced_by_owner": True, "enabled": True}

    for label, value in (
        ("NaN", float("nan")),
        ("positive infinity", float("inf")),
        ("negative infinity", float("-inf")),
    ):
        expect_invalid(
            f"canonical {label}", lambda value=value: tool.canonical_json({"x": value})
        )
        constant = {
            "NaN": b"NaN",
            "positive infinity": b"Infinity",
            "negative infinity": b"-Infinity",
        }[label]
        expect_invalid(
            f"decoded {label}",
            lambda constant=constant: tool.decode_comment(
                PREFLIGHT_MARKER,
                PREFLIGHT_MARKER + b'{"x":' + constant + b"}\n",
                "record",
            ),
        )

    for label, body in (
        ("byte before marker", b"x" + PREFLIGHT_MARKER + canonical(value)),
        ("wrong marker case", b"<!-- Moor-release-preflight-v1 -->\n" + canonical(value)),
        ("missing trailing LF", PREFLIGHT_MARKER + canonical(value)[:-1]),
        ("extra final LF", PREFLIGHT_MARKER + canonical(value) + b"\n"),
        (
            "pretty JSON",
            PREFLIGHT_MARKER
            + (json.dumps(value, indent=2, ensure_ascii=True) + "\n").encode("ascii"),
        ),
        (
            "duplicate keys",
            PREFLIGHT_MARKER + b'{"nonce":"' + b"b" * 64 + b'","nonce":"' + b"c" * 64 + b'"}\n',
        ),
        (
            "Unicode/non-ASCII",
            PREFLIGHT_MARKER + '{"name":"caf\u00e9"}\n'.encode("utf-8"),
        ),
    ):
        expect_invalid(label, lambda body=body: tool.decode_comment(PREFLIGHT_MARKER, body, "record"))

    for label, response in (
        ("empty settings", b""),
        ("disabled settings", b'{"enabled":false,"enforced_by_owner":false}'),
        ("missing settings key", b'{"enabled":true}'),
        ("extra settings key", b'{"enabled":true,"enforced_by_owner":false,"x":1}'),
        ("nonboolean enabled", b'{"enabled":1,"enforced_by_owner":false}'),
        ("nonboolean owner", b'{"enabled":true,"enforced_by_owner":0}'),
        ("duplicate settings key", b'{"enabled":true,"enabled":true,"enforced_by_owner":false}'),
        ("non-ASCII settings", '{"enabled":true,"enforced_by_owner":false,"x":"\u00e9"}'.encode("utf-8")),
    ):
        expect_invalid(label, lambda response=response: tool.validate_settings_response(response))


def test_manifest(tool):
    with tempfile.TemporaryDirectory(prefix="release-promotion-manifest-") as root:
        created, qa, assets, paths = run_manifest(root)
        assert created.returncode == 0, created.stderr.decode(errors="replace")
        with open(paths["manifest"], "rb") as handle:
            first = handle.read()
        manifest = json.loads(first)
        assert first == canonical(manifest)
        assert set(manifest) == {
            "schemaVersion",
            "kind",
            "repository",
            "promotion",
            "qa",
            "candidate",
            "release",
            "assets",
        }
        assert manifest["schemaVersion"] == 1
        assert manifest["kind"] == "moor-release-promotion-manifest-v1"
        assert manifest["repository"] == REPOSITORY
        assert manifest["promotion"] == {
            "workflowRunId": PROMOTION_RUN_ID,
            "workflowRunAttempt": 1,
            "headSha": HEAD_SHA,
            "mode": "promote",
            "nonce": NONCE,
        }
        assert manifest["qa"]["releaseQa"]["artifactId"] == QA_ARTIFACT_ID
        assert manifest["qa"]["candidateQa"]["artifactId"] == qa["candidateQa"][
            "evidenceArtifactId"
        ]
        expected_candidate_artifacts = sorted(
            candidate_artifacts(qa), key=lambda item: item[1].encode("ascii")
        )
        assert [
            (item["id"], item["name"])
            for item in manifest["candidate"]["artifacts"]
        ] == expected_candidate_artifacts
        assert all(
            item["apiDigest"].startswith("sha256:")
            for item in manifest["candidate"]["artifacts"]
        )
        assert manifest["release"] == {
            "version": VERSION,
            "tag": VERSION,
            "name": f"Moor {VERSION}",
            "bodySha256": hashlib.sha256(release_body(qa)).hexdigest(),
        }
        assert manifest["assets"] == assets
        assert len(manifest["assets"]) == 6
        assert "bundleArtifactId" not in first.decode("ascii")
        assert "bundleApiDigest" not in first.decode("ascii")

        verified = invoke(
            ["manifest", "verify"]
            + manifest_common(paths, qa)
            + ["--manifest", paths["manifest"]]
        )
        assert verified.returncode == 0, verified.stderr.decode(errors="replace")
        assert b"promotion manifest verified" in verified.stdout

        repeated = invoke(
            ["manifest", "create"]
            + manifest_common(paths, qa)
            + ["--out", paths["manifest"]]
        )
        assert repeated.returncode == 0, repeated.stderr.decode(errors="replace")
        with open(paths["manifest"], "rb") as handle:
            assert handle.read() == first

    def reject_inputs(label, mutate=None, **overrides):
        with tempfile.TemporaryDirectory(
            prefix="release-promotion-manifest-reject-"
        ) as root:
            qa = make_qa_record()
            assets = make_expected_assets(qa)
            body = release_body(qa)
            if mutate is not None:
                mutate(qa, assets)
            result, _, _, _ = run_manifest(
                root, qa=qa, assets=assets, body=body, **overrides
            )
            assert result.returncode == 1, (
                f"{label}: expected exit 1, got {result.returncode}: "
                f"{result.stderr.decode(errors='replace')}"
            )
            assert result.stderr.strip(), f"{label}: no diagnostic"
            assert result.stderr.startswith(b"release-promotion-record: "), (
                f"{label}: refusal did not use the tool diagnostic prefix: "
                f"{result.stderr.decode(errors='replace')}"
            )
            assert b"Traceback" not in result.stderr, f"{label}: refusal crashed"

    reject_inputs("promotion rerun", promotion_run_attempt="2")
    reject_inputs("release QA rerun", qa_run_attempt="2")
    reject_inputs(
        "candidate rerun",
        lambda qa, assets: qa["candidate"].update(workflowRunAttempt=2),
    )
    reject_inputs(
        "candidate-QA rerun",
        lambda qa, assets: qa["candidateQa"].update(workflowRunAttempt=2),
    )
    reject_inputs(
        "foreign QA target key",
        lambda qa, assets: qa["targets"].__setitem__(
            "foreign-target", qa["targets"].pop(next(iter(qa["targets"])))
        ),
    )
    reject_inputs(
        "foreign target artifact name",
        lambda qa, assets: next(iter(qa["targets"].values())).update(
            artifactName="arbitrary-artifact"
        ),
    )
    reject_inputs(
        "cross-role target artifact name",
        lambda qa, assets: next(iter(qa["targets"].values())).update(
            artifactName="moor-release-candidate-qa-evidence"
        ),
    )
    reject_inputs(
        "non-object manual QA",
        lambda qa, assets: qa.update(manualQa=[]),
    )
    reject_inputs(
        "non-object target manual QA",
        lambda qa, assets: next(iter(qa["targets"].values())).update(
            manualQa=[]
        ),
    )
    reject_inputs(
        "boolean artifact ID",
        lambda qa, assets: qa["candidate"].update(metadataArtifactId=True),
    )
    reject_inputs(
        "boolean asset size", lambda qa, assets: assets[0].update(size=True)
    )
    reject_inputs(
        "malformed asset digest",
        lambda qa, assets: assets[0].update(sha256="ABC"),
    )
    reject_inputs("missing asset", lambda qa, assets: assets.pop())
    reject_inputs(
        "extra asset",
        lambda qa, assets: assets.append(
            {"name": "foreign", "size": 1, "sha256": "f" * 64}
        ),
    )
    reject_inputs(
        "duplicate asset", lambda qa, assets: assets.__setitem__(1, assets[0])
    )
    reject_inputs(
        "foreign asset",
        lambda qa, assets: assets[0].update(name="foreign-release-file"),
    )
    reject_inputs(
        "unsorted assets", lambda qa, assets: assets.reverse()
    )
    reject_inputs(
        "hostile asset name", lambda qa, assets: assets[0].update(name="../asset")
    )
    reject_inputs(
        "non-ASCII asset name", lambda qa, assets: assets[0].update(name="café")
    )
    reject_inputs("wrong candidate tuple", qa_run_id="999")

    with tempfile.TemporaryDirectory(
        prefix="release-promotion-manifest-bad-input-"
    ) as root:
        qa, _, paths = manifest_paths(root)
        write_bytes(paths["assets"], legacy_canonical(make_expected_assets(qa)))
        result = invoke(
            ["manifest", "create"]
            + manifest_common(paths, qa)
            + ["--out", paths["manifest"]]
        )
        assert result.returncode == 1 and result.stderr.strip(), (
            "pretty/noncanonical expected-assets input was accepted"
        )

    with tempfile.TemporaryDirectory(
        prefix="release-promotion-manifest-bad-body-"
    ) as root:
        qa = make_qa_record()
        result, _, _, _ = run_manifest(root, qa=qa, body=release_body(qa) + b"\n")
        assert result.returncode == 1 and result.stderr.strip(), (
            "release body with a final LF was accepted"
        )

    with tempfile.TemporaryDirectory(
        prefix="release-promotion-manifest-bad-api-digest-"
    ) as root:
        _, qa, _, paths = run_manifest(root)
        arguments = ["manifest", "create"] + manifest_common(paths, qa)
        position = arguments.index("--artifact-api-digest") + 1
        arguments[position] = arguments[position].split("=", 1)[0] + "=sha256:BAD"
        result = invoke(arguments + ["--out", paths["manifest"]])
        assert result.returncode == 1 and result.stderr.strip(), (
            "malformed artifact API digest was accepted"
        )

    with tempfile.TemporaryDirectory(
        prefix="release-promotion-manifest-tamper-"
    ) as root:
        created, qa, _, paths = run_manifest(root)
        assert created.returncode == 0, created.stderr.decode(errors="replace")
        with open(paths["manifest"], "rb") as handle:
            valid = json.load(handle)
        mutations = {
            "wrong candidate tuple": lambda value: value["candidate"].update(
                workflowRunId="1"
            ),
            "boolean manifest attempt": lambda value: value["promotion"].update(
                workflowRunAttempt=True
            ),
            "unsorted manifest assets": lambda value: value["assets"].reverse(),
            "extra manifest key": lambda value: value.update(extra=True),
        }
        for label, mutate in mutations.items():
            value = copy.deepcopy(valid)
            mutate(value)
            write_bytes(paths["manifest"], canonical(value))
            result = invoke(
                ["manifest", "verify"]
                + manifest_common(paths, qa)
                + ["--manifest", paths["manifest"]]
            )
            assert result.returncode == 1 and result.stderr.strip(), (
                f"{label}: tampered manifest was accepted"
            )
        write_bytes(paths["manifest"], legacy_canonical(valid))
        result = invoke(
            ["manifest", "verify"]
            + manifest_common(paths, qa)
            + ["--manifest", paths["manifest"]]
        )
        assert result.returncode == 1 and result.stderr.strip(), (
            "pretty/noncanonical manifest was accepted"
        )
        duplicate = canonical(valid).replace(
            b'{"assets":', b'{"schemaVersion":1,"assets":', 1
        )
        write_bytes(paths["manifest"], duplicate)
        result = invoke(
            ["manifest", "verify"]
            + manifest_common(paths, qa)
            + ["--manifest", paths["manifest"]]
        )
        assert result.returncode == 1 and result.stderr.strip(), (
            "duplicate manifest key was accepted"
        )


def prepare_record_fixture(root, source_mode="run-bundle"):
    os.makedirs(root, exist_ok=True)
    created, qa, assets, paths = run_manifest(root)
    assert created.returncode == 0, created.stderr.decode(errors="replace")
    with open(paths["manifest"], "rb") as handle:
        manifest_body = handle.read()
    manifest = json.loads(manifest_body)
    paths.update(
        {
            "settings": os.path.join(root, "immutable-settings.json"),
            "published_assets": os.path.join(root, "published-assets.json"),
            "preflight": os.path.join(root, "preflight-comment.txt"),
            "completion": os.path.join(root, "completion-comment.txt"),
            "comments": os.path.join(root, "comments.json"),
        }
    )
    write_bytes(paths["settings"], SETTINGS_RESPONSE)
    published_assets = [
        {
            "id": 8000 + index,
            "name": entry["name"],
            "size": entry["size"],
            "sha256": entry["sha256"],
        }
        for index, entry in enumerate(assets)
    ]
    write_bytes(paths["published_assets"], canonical(published_assets))
    return {
        "qa": qa,
        "assets": assets,
        "publishedAssets": published_assets,
        "manifest": manifest,
        "manifestBody": manifest_body,
        "paths": paths,
        "sourceMode": source_mode,
    }


def source_arguments(fixture, source_mode=None):
    mode = fixture["sourceMode"] if source_mode is None else source_mode
    arguments = ["--source-mode", mode]
    if mode == "run-bundle":
        arguments += [
            "--source-run-id",
            PROMOTION_RUN_ID,
            "--source-run-attempt",
            "1",
            "--source-artifact-id",
            "7000000001",
            "--source-artifact-name",
            "moor-release-promotion-v1",
            "--source-api-digest",
            "sha256:" + "7" * 64,
        ]
    return arguments


def record_common(fixture, completion=False, **overrides):
    values = {
        "promotion_manifest": fixture["paths"]["manifest"],
        "issue_number": ISSUE_NUMBER,
        "dispatcher": DISPATCHER,
        "administrator": DISPATCHER,
        "gate_ready_at": PREFLIGHT_GATE_READY_AT,
        "github_server_time": (
            COMPLETION_SERVER_TIME if completion else PREFLIGHT_SERVER_TIME
        ),
        "settings_checked_at": (
            COMPLETION_CHECKED_AT if completion else PREFLIGHT_CHECKED_AT
        ),
        "settings_response": fixture["paths"]["settings"],
        "helper_commit": HELPER_SHA,
    }
    values.update(overrides)
    arguments = []
    for name, value in values.items():
        arguments.extend([f"--{name.replace('_', '-')}", str(value)])
    return arguments + source_arguments(fixture)


def preflight_create(fixture, **overrides):
    arguments = ["preflight", "create"] + record_common(fixture, **overrides)
    arguments += ["--out", fixture["paths"]["preflight"]]
    return invoke(arguments)


def completion_common(fixture, preflight_body, **overrides):
    preflight_sha = hashlib.sha256(preflight_body).hexdigest()
    values = {
        "authority_phase": "prepublish",
        "preflight_comment_id": str(COMMENT_ID),
        "preflight_comment_url": (
            "https://github.com/BrainyBlaze/moor/issues/26"
            f"#issuecomment-{COMMENT_ID}"
        ),
        "preflight_body_sha256": preflight_sha,
        "tag_ref": f"refs/tags/{VERSION}",
        "tag_sha": CANDIDATE_SHA,
        "release_id": "9001",
        "release_url": f"https://github.com/{REPOSITORY}/releases/tag/{VERSION}",
        "release_tag": VERSION,
        "release_name": f"Moor {VERSION}",
        "release_body_sha256": fixture["manifest"]["release"]["bodySha256"],
        "release_draft": "false",
        "release_immutable": "true",
        "published_assets": fixture["paths"]["published_assets"],
        "transaction_evidence_manifest_sha256": "8" * 64,
    }
    values.update(overrides)
    arguments = record_common(fixture, completion=True)
    for name, value in values.items():
        arguments.extend([f"--{name.replace('_', '-')}", str(value)])
    return arguments


def completion_create(fixture, preflight_body, **overrides):
    arguments = ["completion", "create"] + completion_common(
        fixture, preflight_body, **overrides
    )
    arguments += ["--out", fixture["paths"]["completion"]]
    return invoke(arguments)


def comment(body, *, completion=False, **overrides):
    comment_id = COMMENT_ID + (1 if completion else 0)
    value = {
        "id": comment_id,
        "html_url": (
            f"https://github.com/{REPOSITORY}/issues/{ISSUE_NUMBER}"
            f"#issuecomment-{comment_id}"
        ),
        "issue_url": (
            f"https://api.github.com/repos/{REPOSITORY}/issues/{ISSUE_NUMBER}"
        ),
        "user": {"login": DISPATCHER},
        "created_at": (
            COMPLETION_COMMENT_TIME if completion else PREFLIGHT_COMMENT_TIME
        ),
        "updated_at": (
            COMPLETION_COMMENT_TIME if completion else PREFLIGHT_COMMENT_TIME
        ),
        "body": body.decode("ascii") if isinstance(body, bytes) else body,
    }
    value.update(overrides)
    return value


def verify_record(
    fixture,
    kind,
    comments,
    preflight_body=None,
    now=None,
    final_recheck=False,
    expected_comment_id=None,
    expected_body_sha256=None,
    common_overrides=None,
):
    write_bytes(
        fixture["paths"]["comments"],
        json.dumps(comments, ensure_ascii=False).encode("utf-8"),
    )
    if kind == "preflight":
        arguments = [kind, "verify"] + record_common(
            fixture, **(common_overrides or {})
        )
        now = PREFLIGHT_NOW if now is None else now
    else:
        arguments = [kind, "verify"] + completion_common(
            fixture, preflight_body, **(common_overrides or {})
        )
        now = COMPLETION_NOW if now is None else now
    arguments += [
        "--comments",
        fixture["paths"]["comments"],
        "--expected-author",
        DISPATCHER,
        "--now",
        now,
    ]
    if final_recheck:
        arguments.append("--final-recheck")
    if expected_comment_id is not None:
        arguments += ["--expected-comment-id", str(expected_comment_id)]
    if expected_body_sha256 is not None:
        arguments += ["--expected-body-sha256", expected_body_sha256]
    return invoke(arguments)


def assert_record_rejected(label, result, expected=1):
    assert result.returncode == expected, (
        f"{label}: expected exit {expected}, got {result.returncode}: "
        f"{result.stderr.decode(errors='replace')}"
    )
    assert result.stderr.strip(), f"{label}: no diagnostic"
    if expected == 1:
        assert result.stderr.startswith(b"release-promotion-record: "), (
            f"{label}: refusal did not use the tool diagnostic prefix: "
            f"{result.stderr.decode(errors='replace')}"
        )
        assert b"Traceback" not in result.stderr, f"{label}: refusal crashed"


def mutate_record_body(marker, body, mutate):
    value = json.loads(body[len(marker) :])
    mutate(value)
    return marker + canonical(value)


def test_preflight_completion(tool):
    skew = tool.CLOCK_SKEW_SECONDS
    freshness = tool.COMMENT_FRESHNESS_SECONDS
    with tempfile.TemporaryDirectory(prefix="release-promotion-comments-") as root:
        fixture = prepare_record_fixture(root)
        created = preflight_create(fixture)
        assert created.returncode == 0, created.stderr.decode(errors="replace")
        preflight_body = open(fixture["paths"]["preflight"], "rb").read()
        assert_marker(preflight_body, PREFLIGHT_MARKER)
        preflight = json.loads(preflight_body[len(PREFLIGHT_MARKER) :])
        assert set(preflight) == {
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
        }
        assert preflight["kind"] == "moor-release-preflight-v1"
        assert preflight["promotion"] == {
            **fixture["manifest"]["promotion"],
            "issueNumber": ISSUE_NUMBER,
            "gateReadyAt": PREFLIGHT_GATE_READY_AT,
        }
        assert preflight["qa"] == fixture["manifest"]["qa"]
        assert preflight["candidate"] == fixture["manifest"]["candidate"]
        assert preflight["release"] == fixture["manifest"]["release"]
        assert preflight["manifestSha256"] == hashlib.sha256(
            fixture["manifestBody"]
        ).hexdigest()
        assert preflight["source"] == {
            "mode": "run-bundle",
            "workflowRunId": PROMOTION_RUN_ID,
            "workflowRunAttempt": 1,
            "artifactId": 7000000001,
            "artifactName": "moor-release-promotion-v1",
            "apiDigest": "sha256:" + "7" * 64,
        }
        assert preflight["dispatcher"] == DISPATCHER
        assert preflight["authority"] == {
            "administrator": DISPATCHER,
            "gateReadyAt": PREFLIGHT_GATE_READY_AT,
            "githubServerTime": PREFLIGHT_SERVER_TIME,
            "immutableReleaseSettings": {
                "checkedAt": PREFLIGHT_CHECKED_AT,
                "responseBase64": base64.b64encode(SETTINGS_RESPONSE).decode("ascii"),
                "responseSha256": hashlib.sha256(SETTINGS_RESPONSE).hexdigest(),
            },
        }
        assert preflight["helperCommit"] == HELPER_SHA

        unrelated = {"body": "unrelated café"}
        valid_comment = comment(preflight_body)
        accepted = verify_record(
            fixture, "preflight", [unrelated, valid_comment, {"body": "after"}]
        )
        assert accepted.returncode == 0, accepted.stderr.decode(errors="replace")
        snapshot = json.loads(accepted.stdout)
        assert accepted.stdout == canonical(snapshot)
        assert snapshot == {
            "schemaVersion": 1,
            "commentId": str(COMMENT_ID),
            "commentUrl": valid_comment["html_url"],
            "author": DISPATCHER,
            "createdAt": PREFLIGHT_COMMENT_TIME,
            "updatedAt": PREFLIGHT_COMMENT_TIME,
            "bodySha256": hashlib.sha256(preflight_body).hexdigest(),
            "record": preflight,
        }

        completed = completion_create(fixture, preflight_body)
        assert completed.returncode == 0, completed.stderr.decode(errors="replace")
        completion_body = open(fixture["paths"]["completion"], "rb").read()
        assert_marker(completion_body, COMPLETION_MARKER)
        completion = json.loads(completion_body[len(COMPLETION_MARKER) :])
        assert completion["kind"] == "moor-release-completion-v1"
        assert completion["promotion"] == preflight["promotion"]
        assert completion["qa"] == preflight["qa"]
        assert completion["candidate"] == preflight["candidate"]
        assert completion["source"] == preflight["source"]
        assert completion["manifestSha256"] == preflight["manifestSha256"]
        assert completion["preflight"] == {
            "commentId": COMMENT_ID,
            "commentUrl": valid_comment["html_url"],
            "bodySha256": hashlib.sha256(preflight_body).hexdigest(),
        }
        assert completion["tag"] == {
            "ref": f"refs/tags/{VERSION}",
            "targetSha": CANDIDATE_SHA,
        }
        assert completion["release"] == {
            "id": 9001,
            "url": f"https://github.com/{REPOSITORY}/releases/tag/{VERSION}",
            "version": VERSION,
            "tag": VERSION,
            "name": f"Moor {VERSION}",
            "bodySha256": fixture["manifest"]["release"]["bodySha256"],
            "targetCommit": CANDIDATE_SHA,
            "draft": False,
            "immutable": True,
        }
        assert completion["assets"] == fixture["publishedAssets"]
        assert completion["authority"]["phase"] == "prepublish"
        assert completion["transactionEvidenceManifestSha256"] == "8" * 64
        completion_comment = comment(completion_body, completion=True)
        completion_accepted = verify_record(
            fixture,
            "completion",
            [unrelated, completion_comment],
            preflight_body=preflight_body,
        )
        assert completion_accepted.returncode == 0, completion_accepted.stderr.decode(
            errors="replace"
        )
        completion_snapshot = json.loads(completion_accepted.stdout)
        assert completion_snapshot["commentId"] == str(COMMENT_ID + 1)
        assert completion_snapshot["bodySha256"] == hashlib.sha256(
            completion_body
        ).hexdigest()
        assert completion_snapshot["record"] == completion

        recovered = completion_create(
            fixture, preflight_body, authority_phase="published-recovery"
        )
        assert recovered.returncode == 0, recovered.stderr.decode(errors="replace")
        recovered_body = open(fixture["paths"]["completion"], "rb").read()
        recovered_value = json.loads(recovered_body[len(COMPLETION_MARKER) :])
        assert recovered_value["authority"]["phase"] == "published-recovery"

    with tempfile.TemporaryDirectory(
        prefix="release-promotion-reconstruction-"
    ) as root:
        fixture = prepare_record_fixture(root, source_mode="qa-reconstruction")
        created = preflight_create(fixture)
        assert created.returncode == 0, created.stderr.decode(errors="replace")
        body = open(fixture["paths"]["preflight"], "rb").read()
        value = json.loads(body[len(PREFLIGHT_MARKER) :])
        assert value["source"] == {
            "mode": "qa-reconstruction",
            "candidate": fixture["manifest"]["candidate"],
            "qa": fixture["manifest"]["qa"],
        }
        accepted = verify_record(fixture, "preflight", [comment(body)])
        assert accepted.returncode == 0, accepted.stderr.decode(errors="replace")

    with tempfile.TemporaryDirectory(
        prefix="release-promotion-comment-refusals-"
    ) as root:
        fixture = prepare_record_fixture(root)
        created = preflight_create(fixture)
        assert created.returncode == 0, created.stderr.decode(errors="replace")
        preflight_body = open(fixture["paths"]["preflight"], "rb").read()
        valid = comment(preflight_body)

        assert_record_rejected(
            "missing preflight",
            verify_record(fixture, "preflight", []),
            expected=75,
        )
        assert_record_rejected(
            "non-JSON constant in comments",
            verify_record(fixture, "preflight", [float("nan")]),
        )
        other_nonce = mutate_record_body(
            PREFLIGHT_MARKER,
            preflight_body,
            lambda value: value["promotion"].update(nonce="f" * 64),
        )
        assert_record_rejected(
            "unrelated nonce",
            verify_record(fixture, "preflight", [comment(other_nonce)]),
            expected=75,
        )
        assert_record_rejected(
            "same-nonce duplicate",
            verify_record(
                fixture,
                "preflight",
                [valid, dict(valid, id=COMMENT_ID + 10)],
            ),
        )
        assert_record_rejected(
            "matching wrong author",
            verify_record(
                fixture,
                "preflight",
                [comment(preflight_body, user={"login": "other-admin"})],
            ),
        )
        assert_record_rejected(
            "edited preflight",
            verify_record(
                fixture,
                "preflight",
                [comment(preflight_body, updated_at=shifted(PREFLIGHT_COMMENT_TIME, 1))],
            ),
        )
        assert_record_rejected(
            "boolean comment ID",
            verify_record(
                fixture, "preflight", [comment(preflight_body, id=True)]
            ),
        )
        assert_record_rejected(
            "foreign issue URL",
            verify_record(
                fixture,
                "preflight",
                [comment(preflight_body, issue_url="https://example.com/issues/26")],
            ),
        )
        assert_record_rejected(
            "foreign comment URL",
            verify_record(
                fixture,
                "preflight",
                [comment(preflight_body, html_url="https://example.com/comment")],
            ),
        )
        malformed = (
            PREFLIGHT_MARKER.decode("ascii")
            + '{"kind":"moor-release-preflight-v1","promotion":{"nonce":"'
            + NONCE
            + '"}'
        )
        assert_record_rejected(
            "matching malformed preflight",
            verify_record(fixture, "preflight", [comment(malformed)]),
        )

        byte_mutations = {
            "byte before marker": b"x" + preflight_body,
            "wrong marker case": preflight_body.replace(b"moor", b"Moor", 1),
            "missing final LF": preflight_body[:-1],
            "extra final LF": preflight_body + b"\n",
            "pretty body": PREFLIGHT_MARKER + legacy_canonical(preflight),
            "non-ASCII body": (
                preflight_body.decode("ascii")[:-1] + "é\n"
            ),
        }
        duplicate_body = preflight_body.replace(
            b'{"authority":', b'{"schemaVersion":1,"authority":', 1
        )
        byte_mutations["duplicate JSON key"] = duplicate_body
        for label, body in byte_mutations.items():
            assert_record_rejected(
                label,
                verify_record(fixture, "preflight", [comment(body)]),
            )

        tuple_mutations = {
            "promotion run": lambda value: value["promotion"].update(
                workflowRunId="1"
            ),
            "promotion head": lambda value: value["promotion"].update(
                headSha="0" * 40
            ),
            "release-QA tuple": lambda value: value["qa"]["releaseQa"].update(
                artifactId="1"
            ),
            "candidate tuple": lambda value: value["candidate"].update(
                commit="0" * 40
            ),
            "source tuple": lambda value: value["source"].update(artifactId=1),
            "manifest digest": lambda value: value.update(manifestSha256="0" * 64),
            "dispatcher": lambda value: value.update(dispatcher="other-admin"),
            "administrator": lambda value: value["authority"].update(
                administrator="other-admin"
            ),
            "helper commit": lambda value: value.update(helperCommit="0" * 40),
            "extra key": lambda value: value.update(extra=True),
        }
        for label, mutate in tuple_mutations.items():
            body = mutate_record_body(PREFLIGHT_MARKER, preflight_body, mutate)
            assert_record_rejected(
                label,
                verify_record(fixture, "preflight", [comment(body)]),
            )

        settings_tamper = json.loads(preflight_body[len(PREFLIGHT_MARKER) :])
        alternate = b'{"enabled":true,"enforced_by_owner":true}'
        settings_tamper["authority"]["immutableReleaseSettings"].update(
            responseBase64=base64.b64encode(alternate).decode("ascii"),
            responseSha256=hashlib.sha256(alternate).hexdigest(),
        )
        assert_record_rejected(
            "settings exact bytes",
            verify_record(
                fixture,
                "preflight",
                [comment(PREFLIGHT_MARKER + canonical(settings_tamper))],
            ),
        )

        boundary_times = {
            "settings_checked_at": PREFLIGHT_GATE_READY_AT,
            "github_server_time": PREFLIGHT_GATE_READY_AT,
        }
        boundary_created = preflight_create(fixture, **boundary_times)
        assert boundary_created.returncode == 0, boundary_created.stderr.decode(
            errors="replace"
        )
        boundary_body = open(fixture["paths"]["preflight"], "rb").read()
        for delta, accepted in (
            (-skew - 1, False),
            (-skew, True),
            (-skew + 1, True),
        ):
            created_at = shifted(PREFLIGHT_GATE_READY_AT, delta)
            result = verify_record(
                fixture,
                "preflight",
                [
                    comment(
                        boundary_body,
                        created_at=created_at,
                        updated_at=created_at,
                    )
                ],
                now=PREFLIGHT_NOW,
                common_overrides=boundary_times,
            )
            if accepted:
                assert result.returncode == 0, result.stderr.decode(errors="replace")
            else:
                assert_record_rejected("comment before gate boundary", result)

        freshness_times = {
            "settings_checked_at": PREFLIGHT_COMMENT_TIME,
            "github_server_time": PREFLIGHT_COMMENT_TIME,
        }
        freshness_created = preflight_create(fixture, **freshness_times)
        assert freshness_created.returncode == 0, freshness_created.stderr.decode(
            errors="replace"
        )
        freshness_body = open(fixture["paths"]["preflight"], "rb").read()
        freshness_comment = comment(freshness_body)
        for age, accepted in (
            (freshness - 1, True),
            (freshness, True),
            (freshness + 1, False),
        ):
            result = verify_record(
                fixture,
                "preflight",
                [freshness_comment],
                now=shifted(PREFLIGHT_COMMENT_TIME, age),
                common_overrides=freshness_times,
            )
            if accepted:
                assert result.returncode == 0, result.stderr.decode(errors="replace")
            else:
                assert_record_rejected("comment freshness boundary", result)

        for lead, accepted in (
            (skew - 1, True),
            (skew, True),
            (skew + 1, False),
        ):
            result = verify_record(
                fixture,
                "preflight",
                [valid],
                now=shifted(PREFLIGHT_COMMENT_TIME, -lead),
            )
            if accepted:
                assert result.returncode == 0, result.stderr.decode(errors="replace")
            else:
                assert_record_rejected("comment future boundary", result)

        for lag, accepted in (
            (skew - 1, True),
            (skew, True),
            (skew + 1, False),
        ):
            authority_time = shifted(PREFLIGHT_COMMENT_TIME, lag)
            authority_times = {
                "settings_checked_at": authority_time,
                "github_server_time": authority_time,
            }
            boundary_created = preflight_create(fixture, **authority_times)
            assert boundary_created.returncode == 0, boundary_created.stderr.decode(
                errors="replace"
            )
            authority_body = open(fixture["paths"]["preflight"], "rb").read()
            result = verify_record(
                fixture,
                "preflight",
                [comment(authority_body)],
                common_overrides=authority_times,
            )
            if accepted:
                assert result.returncode == 0, result.stderr.decode(errors="replace")
            else:
                assert_record_rejected("settings/comment skew boundary", result)

        authority_times = {
            "settings_checked_at": PREFLIGHT_CHECKED_AT,
            "github_server_time": PREFLIGHT_CHECKED_AT,
        }
        boundary_created = preflight_create(fixture, **authority_times)
        assert boundary_created.returncode == 0, boundary_created.stderr.decode(
            errors="replace"
        )
        authority_body = open(fixture["paths"]["preflight"], "rb").read()
        for age, accepted in (
            (freshness - 1, True),
            (freshness, True),
            (freshness + 1, False),
        ):
            now = shifted(PREFLIGHT_CHECKED_AT, age)
            authority_comment = comment(
                authority_body, created_at=now, updated_at=now
            )
            result = verify_record(
                fixture,
                "preflight",
                [authority_comment],
                now=now,
                common_overrides=authority_times,
            )
            if accepted:
                assert result.returncode == 0, result.stderr.decode(errors="replace")
            else:
                assert_record_rejected("settings freshness boundary", result)

        final_sha = hashlib.sha256(preflight_body).hexdigest()
        final = verify_record(
            fixture,
            "preflight",
            valid,
            now=shifted(PREFLIGHT_COMMENT_TIME, 3600),
            final_recheck=True,
            expected_comment_id=COMMENT_ID,
            expected_body_sha256=final_sha,
        )
        assert final.returncode == 0, final.stderr.decode(errors="replace")
        assert_record_rejected(
            "final recheck missing identity",
            verify_record(
                fixture,
                "preflight",
                [valid],
                now=shifted(PREFLIGHT_COMMENT_TIME, 3600),
                final_recheck=True,
            ),
        )
        assert_record_rejected(
            "final recheck ID changed",
            verify_record(
                fixture,
                "preflight",
                [valid],
                final_recheck=True,
                expected_comment_id=COMMENT_ID + 1,
                expected_body_sha256=final_sha,
            ),
        )
        assert_record_rejected(
            "final recheck body changed",
            verify_record(
                fixture,
                "preflight",
                [valid],
                final_recheck=True,
                expected_comment_id=COMMENT_ID,
                expected_body_sha256="0" * 64,
            ),
        )
        assert_record_rejected(
            "final recheck still rejects edits",
            verify_record(
                fixture,
                "preflight",
                [comment(preflight_body, updated_at=shifted(PREFLIGHT_COMMENT_TIME, 1))],
                final_recheck=True,
                expected_comment_id=COMMENT_ID,
                expected_body_sha256=final_sha,
            ),
        )
        assert_record_rejected(
            "final recheck still rejects author changes",
            verify_record(
                fixture,
                "preflight",
                [comment(preflight_body, user={"login": "other-admin"})],
                final_recheck=True,
                expected_comment_id=COMMENT_ID,
                expected_body_sha256=final_sha,
            ),
        )
        future_time = shifted(PREFLIGHT_NOW, skew + 1)
        assert_record_rejected(
            "final recheck still rejects future comments",
            verify_record(
                fixture,
                "preflight",
                [
                    comment(
                        preflight_body,
                        created_at=future_time,
                        updated_at=future_time,
                    )
                ],
                now=PREFLIGHT_NOW,
                final_recheck=True,
                expected_comment_id=COMMENT_ID,
                expected_body_sha256=final_sha,
            ),
        )

    with tempfile.TemporaryDirectory(
        prefix="release-promotion-authority-boundaries-"
    ) as root:
        for delta, accepted in (
            (-skew - 1, False),
            (-skew, True),
            (-skew + 1, True),
        ):
            fixture = prepare_record_fixture(os.path.join(root, f"gate-{delta}"))
            checked = shifted(PREFLIGHT_GATE_READY_AT, delta)
            result = preflight_create(
                fixture,
                settings_checked_at=checked,
                github_server_time=checked,
            )
            if accepted:
                assert result.returncode == 0, result.stderr.decode(errors="replace")
            else:
                assert_record_rejected("settings-before-gate boundary", result)

        for delta in (
            -skew - 1,
            -skew,
            -skew + 1,
            skew - 1,
            skew,
            skew + 1,
        ):
            fixture = prepare_record_fixture(os.path.join(root, f"clock-{delta}"))
            result = preflight_create(
                fixture,
                github_server_time=shifted(PREFLIGHT_CHECKED_AT, delta),
            )
            if abs(delta) <= skew:
                assert result.returncode == 0, result.stderr.decode(errors="replace")
            else:
                assert_record_rejected("GitHub clock-skew boundary", result)

    with tempfile.TemporaryDirectory(
        prefix="release-promotion-completion-refusals-"
    ) as root:
        fixture = prepare_record_fixture(root)
        assert preflight_create(fixture).returncode == 0
        preflight_body = open(fixture["paths"]["preflight"], "rb").read()
        assert completion_create(fixture, preflight_body).returncode == 0
        completion_body = open(fixture["paths"]["completion"], "rb").read()
        valid = comment(completion_body, completion=True)

        assert_record_rejected(
            "missing completion",
            verify_record(
                fixture, "completion", [], preflight_body=preflight_body
            ),
            expected=75,
        )
        assert_record_rejected(
            "duplicate completion",
            verify_record(
                fixture,
                "completion",
                [valid, dict(valid, id=COMMENT_ID + 20)],
                preflight_body=preflight_body,
            ),
        )
        completion_mutations = {
            "completion preflight digest": lambda value: value["preflight"].update(
                bodySha256="0" * 64
            ),
            "completion tag": lambda value: value["tag"].update(targetSha="0" * 40),
            "completion release": lambda value: value["release"].update(id=1),
            "completion boolean release ID": lambda value: value["release"].update(
                id=True
            ),
            "completion source": lambda value: value["source"].update(artifactId=1),
            "completion asset": lambda value: value["assets"][0].update(sha256="0" * 64),
            "completion boolean asset ID": lambda value: value["assets"][0].update(
                id=True
            ),
            "completion assets unsorted": lambda value: value["assets"].reverse(),
            "completion evidence digest": lambda value: value.update(
                transactionEvidenceManifestSha256="0" * 64
            ),
            "completion authority phase": lambda value: value["authority"].update(
                phase="postpublish"
            ),
            "completion public flags": lambda value: value["release"].update(
                draft=True, immutable=False
            ),
        }
        for label, mutate in completion_mutations.items():
            body = mutate_record_body(COMPLETION_MARKER, completion_body, mutate)
            assert_record_rejected(
                label,
                verify_record(
                    fixture,
                    "completion",
                    [comment(body, completion=True)],
                    preflight_body=preflight_body,
                ),
            )

        completion_freshness_times = {
            "settings_checked_at": COMPLETION_COMMENT_TIME,
            "github_server_time": COMPLETION_COMMENT_TIME,
        }
        freshness_created = completion_create(
            fixture, preflight_body, **completion_freshness_times
        )
        assert freshness_created.returncode == 0, freshness_created.stderr.decode(
            errors="replace"
        )
        freshness_body = open(fixture["paths"]["completion"], "rb").read()
        freshness_comment = comment(freshness_body, completion=True)
        for age, accepted in (
            (freshness - 1, True),
            (freshness, True),
            (freshness + 1, False),
        ):
            result = verify_record(
                fixture,
                "completion",
                [freshness_comment],
                preflight_body=preflight_body,
                now=shifted(COMPLETION_COMMENT_TIME, age),
                common_overrides=completion_freshness_times,
            )
            if accepted:
                assert result.returncode == 0, result.stderr.decode(errors="replace")
            else:
                assert_record_rejected("completion freshness boundary", result)

        for lead, accepted in (
            (skew - 1, True),
            (skew, True),
            (skew + 1, False),
        ):
            result = verify_record(
                fixture,
                "completion",
                [valid],
                preflight_body=preflight_body,
                now=shifted(COMPLETION_COMMENT_TIME, -lead),
            )
            if accepted:
                assert result.returncode == 0, result.stderr.decode(errors="replace")
            else:
                assert_record_rejected("completion future boundary", result)

        final_sha = hashlib.sha256(completion_body).hexdigest()
        final = verify_record(
            fixture,
            "completion",
            valid,
            preflight_body=preflight_body,
            now=shifted(COMPLETION_COMMENT_TIME, 3600),
            final_recheck=True,
            expected_comment_id=COMMENT_ID + 1,
            expected_body_sha256=final_sha,
        )
        assert final.returncode == 0, final.stderr.decode(errors="replace")

        assert_record_rejected(
            "invalid completion authority phase input",
            completion_create(fixture, preflight_body, authority_phase="postpublish"),
        )
        assert_record_rejected(
            "mutable completion input",
            completion_create(fixture, preflight_body, release_immutable="false"),
        )
        assert_record_rejected(
            "draft completion input",
            completion_create(fixture, preflight_body, release_draft="true"),
        )

    with tempfile.TemporaryDirectory(
        prefix="release-promotion-strict-source-"
    ) as root:
        fixture = prepare_record_fixture(root, source_mode="qa-reconstruction")
        arguments = ["preflight", "create"] + record_common(fixture)
        arguments += [
            "--source-artifact-id",
            "7",
            "--out",
            fixture["paths"]["preflight"],
        ]
        assert_record_rejected(
            "QA reconstruction optional-field soup", invoke(arguments)
        )

        fixture["sourceMode"] = "run-bundle"
        arguments = ["preflight", "create"] + record_common(fixture)
        position = arguments.index("--source-artifact-id") + 1
        arguments[position] = "true"
        arguments += ["--out", fixture["paths"]["preflight"]]
        assert_record_rejected("boolean run-bundle artifact ID", invoke(arguments))


def test_evidence(tool):
    with open(TOOL, encoding="ascii") as handle:
        evidence_source = handle.read()
    assert "O_NOFOLLOW" in evidence_source, "evidence reads must not follow links"
    assert "dir_fd=" in evidence_source, "evidence traversal must stay descriptor-relative"
    with tempfile.TemporaryDirectory(prefix="release-promotion-evidence-") as root:
        transaction = os.path.join(root, "transaction")
        delivery = os.path.join(root, "delivery")
        os.makedirs(os.path.join(transaction, "nested"))
        os.makedirs(delivery)
        files = {
            "001-request.json": b'{"method":"GET"}\n',
            "nested/002-response.bin": b"response-bytes\x00\xff",
            "nested/003-empty.txt": b"",
        }
        for relative, body in files.items():
            write_bytes(os.path.join(transaction, *relative.split("/")), body)
        write_bytes(os.path.join(delivery, "completion-response.json"), b"delivery")
        out = os.path.join(transaction, "transaction-evidence-manifest.json")
        created = invoke(
            [
                "evidence",
                "create",
                "--transaction-root",
                transaction,
                "--out",
                out,
            ]
        )
        assert created.returncode == 0, created.stderr.decode(errors="replace")
        body = open(out, "rb").read()
        value = json.loads(body)
        assert body == canonical(value)
        assert value == {
            "schemaVersion": 1,
            "kind": "moor-release-transaction-evidence-manifest-v1",
            "files": [
                {
                    "path": relative,
                    "size": len(files[relative]),
                    "sha256": hashlib.sha256(files[relative]).hexdigest(),
                }
                for relative in sorted(files, key=lambda item: item.encode("ascii"))
            ],
        }
        serialized = body.decode("ascii")
        assert "transaction-evidence-manifest.json" not in serialized
        assert "delivery" not in serialized

        repeated = invoke(
            [
                "evidence",
                "create",
                "--transaction-root",
                transaction,
                "--out",
                out,
            ]
        )
        assert repeated.returncode == 0, repeated.stderr.decode(errors="replace")
        assert open(out, "rb").read() == body
        assert tool.validate_evidence_manifest(value) == value

        for label, path in (
            ("absolute path", "/absolute"),
            ("dot component", "a/./b"),
            ("dotdot component", "a/../b"),
            ("empty component", "a//b"),
            ("backslash", "a\\b"),
            ("non-ASCII", "café"),
            ("hidden component", ".secret"),
        ):
            expect_invalid(
                label,
                lambda path=path: tool.validate_evidence_path(path, "test path"),
            )

        invalid_records = []
        unsorted = copy.deepcopy(value)
        unsorted["files"].reverse()
        invalid_records.append(("unsorted inventory", unsorted))
        duplicate = copy.deepcopy(value)
        duplicate["files"][1] = duplicate["files"][0]
        invalid_records.append(("duplicate inventory path", duplicate))
        boolean_size = copy.deepcopy(value)
        boolean_size["files"][0]["size"] = True
        invalid_records.append(("boolean inventory size", boolean_size))
        malformed_digest = copy.deepcopy(value)
        malformed_digest["files"][0]["sha256"] = "BAD"
        invalid_records.append(("malformed inventory digest", malformed_digest))
        extra = copy.deepcopy(value)
        extra["files"][0]["extra"] = True
        invalid_records.append(("extra inventory field", extra))
        for label, record in invalid_records:
            expect_invalid(
                label,
                lambda record=record: tool.validate_evidence_manifest(record),
            )

    def evidence_reject(label, setup, out_inside=False):
        with tempfile.TemporaryDirectory(
            prefix="release-promotion-evidence-reject-"
        ) as root:
            transaction = os.path.join(root, "transaction")
            os.makedirs(transaction)
            setup(root, transaction)
            out = (
                os.path.join(transaction, "transaction-evidence-manifest.json")
                if out_inside
                else os.path.join(root, "transaction-evidence-manifest.json")
            )
            result = invoke(
                [
                    "evidence",
                    "create",
                    "--transaction-root",
                    transaction,
                    "--out",
                    out,
                ]
            )
            assert_record_rejected(label, result)

    if hasattr(os, "symlink"):
        evidence_reject(
            "symlink file",
            lambda root, transaction: os.symlink(
                os.path.join(root, "target"), os.path.join(transaction, "link")
            ),
        )

        def symlink_directory(root, transaction):
            target = os.path.join(root, "target-directory")
            os.makedirs(target)
            write_bytes(os.path.join(target, "file"), b"foreign")
            os.symlink(target, os.path.join(transaction, "linked-directory"))

        evidence_reject("symlink directory", symlink_directory)

        with tempfile.TemporaryDirectory(
            prefix="release-promotion-evidence-output-link-"
        ) as root:
            transaction = os.path.join(root, "transaction")
            os.makedirs(transaction)
            write_bytes(os.path.join(transaction, "file"), b"bytes")
            target = os.path.join(root, "target")
            out = os.path.join(root, "transaction-evidence-manifest.json")
            write_bytes(target, b"must-not-change")
            os.symlink(target, out)
            result = invoke(
                [
                    "evidence",
                    "create",
                    "--transaction-root",
                    transaction,
                    "--out",
                    out,
                ]
            )
            assert_record_rejected("symlink evidence output", result)
            assert open(target, "rb").read() == b"must-not-change"

        with tempfile.TemporaryDirectory(
            prefix="release-promotion-evidence-parent-swap-"
        ) as root:
            transaction = os.path.join(root, "transaction")
            moved = os.path.join(root, "moved-transaction")
            redirect = os.path.join(root, "redirect")
            out = os.path.join(transaction, "transaction-evidence-manifest.json")
            os.makedirs(transaction)
            os.makedirs(redirect)
            write_bytes(os.path.join(transaction, "file"), b"bytes")
            value = tool.build_evidence_manifest(transaction, out)
            os.rename(transaction, moved)
            os.symlink(redirect, transaction)
            expect_invalid(
                "replaced evidence output parent",
                lambda: tool.write_evidence_output(out, canonical(value)),
            )
            assert not os.path.exists(
                os.path.join(redirect, "transaction-evidence-manifest.json")
            ), "evidence output followed a replaced parent directory"

    if hasattr(os, "mkfifo"):
        evidence_reject(
            "FIFO entry",
            lambda root, transaction: os.mkfifo(os.path.join(transaction, "pipe")),
        )

    evidence_reject(
        "non-ASCII filesystem path",
        lambda root, transaction: write_bytes(
            os.path.join(transaction, "café"), b"hostile"
        ),
    )
    evidence_reject(
        "hostile filesystem path",
        lambda root, transaction: write_bytes(
            os.path.join(transaction, "line\nbreak"), b"hostile"
        ),
    )

    if hasattr(os, "link"):
        with tempfile.TemporaryDirectory(
            prefix="release-promotion-evidence-output-hardlink-"
        ) as root:
            transaction = os.path.join(root, "transaction")
            os.makedirs(transaction)
            out = os.path.join(root, "transaction-evidence-manifest.json")
            write_bytes(out, b"previous-output")
            os.link(out, os.path.join(transaction, "output-alias"))
            result = invoke(
                [
                    "evidence",
                    "create",
                    "--transaction-root",
                    transaction,
                    "--out",
                    out,
                ]
            )
            assert_record_rejected("hard-linked evidence output alias", result)

    if hasattr(os, "symlink"):
        with tempfile.TemporaryDirectory(
            prefix="release-promotion-evidence-root-link-"
        ) as root:
            real = os.path.join(root, "real")
            linked = os.path.join(root, "transaction")
            os.makedirs(real)
            write_bytes(os.path.join(real, "file"), b"bytes")
            os.symlink(real, linked)
            result = invoke(
                [
                    "evidence",
                    "create",
                    "--transaction-root",
                    linked,
                    "--out",
                    os.path.join(root, "manifest.json"),
                ]
            )
            assert_record_rejected("symlink transaction root", result)


def main():
    tool = load_tool()
    test_primitives(tool)
    test_manifest(tool)
    test_preflight_completion(tool)
    test_evidence(tool)
    print(
        "release promotion record tests: manifest, comments, source modes, "
        "timing boundaries, and evidence inventory accepted; refusal cases passed"
    )


if __name__ == "__main__":
    main()
