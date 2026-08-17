#!/usr/bin/env python3
"""Deterministic acceptance and tamper tests for release-qa-record.py."""

import copy
import hashlib
import importlib.util
import json
import os
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
TOOL = os.path.join(HERE, "release-qa-record.py")
ASSEMBLER = os.path.join(HERE, "candidate-manifest.py")

spec = importlib.util.spec_from_file_location("candidate_manifest", ASSEMBLER)
candidate_manifest = importlib.util.module_from_spec(spec)
spec.loader.exec_module(candidate_manifest)

REPOSITORY = "https://github.com/BrainyBlaze/moor"
VERSION = "v0.1.0"
COMMIT = "a" * 40
RUN_ID = "500"
RUN_ATTEMPT = 1
METADATA_ID = "700"
RECORD_ID = "701"
APPROVED_BY = "levi770"
EVIDENCE_URL = "https://github.com/BrainyBlaze/moor/issues/1#issuecomment-1234567890"
EVIDENCE_COMMENT_ID = "1234567890"
EVIDENCE_TIME = "2026-08-17T09:30:00Z"
EVIDENCE_LINK = "https://github.com/BrainyBlaze/desk/actions/runs/600"

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


def canonical(value):
    return (json.dumps(value, indent=2, ensure_ascii=True) + "\n").encode("ascii")


def write_bytes(path, body):
    with open(path, "wb") as handle:
        handle.write(body)


def confirmation():
    return (
        f"APPROVE MOOR {VERSION} {COMMIT} {RUN_ID}/{RUN_ATTEMPT} "
        f"{METADATA_ID} {RECORD_ID} full-matrix"
    )


def make_manifest():
    targets = {}
    for target_index, target in enumerate(candidate_manifest.TARGETS):
        digest = hashlib.sha256(target.encode("ascii")).hexdigest()
        verification = []
        job_id = 1000 + target_index * 100
        for gate in candidate_manifest.GATE_ORDER:
            for lane in sorted(candidate_manifest.REQUIRED[target].get(gate, set())):
                job_id += 1
                verification.append(
                    {
                        "gate": gate,
                        "lane": lane,
                        "workflowRunId": RUN_ID,
                        "workflowRunAttempt": RUN_ATTEMPT,
                        "jobId": str(job_id),
                        "jobName": f"Verify {target} / {gate} / {lane}",
                    }
                )
        targets[target] = {
            "asset": f"moor-0.1.0-{candidate_manifest.ASSET_SUFFIX[target]}",
            "size": 1_000_000 + target_index,
            "sha256": digest,
            "artifactId": str(600 + target_index),
            "artifactName": f"moor-candidate-{target}",
            "provenance": {
                "build": {
                    "workflowRunId": RUN_ID,
                    "workflowRunAttempt": RUN_ATTEMPT,
                    "jobId": str(10 + target_index),
                    "jobName": f"Build {target}",
                },
                "verification": verification,
            },
        }
    return {
        "schemaVersion": 1,
        "repository": REPOSITORY,
        "version": VERSION,
        "commit": COMMIT,
        "candidate": {
            "workflowRunId": RUN_ID,
            "workflowRunAttempt": RUN_ATTEMPT,
            "metadataArtifactName": "moor-release-candidate-v1",
        },
        "coverage": {"requiredClosure": "full-matrix"},
        "targets": targets,
    }


def make_record(manifest):
    return {
        "repository": REPOSITORY,
        "version": VERSION,
        "commit": COMMIT,
        "workflowRunId": RUN_ID,
        "workflowRunAttempt": RUN_ATTEMPT,
        "metadataArtifactId": METADATA_ID,
        "metadataArtifactName": "moor-release-candidate-v1",
        "targets": [
            {
                "asset": entry["asset"],
                "size": entry["size"],
                "sha256": entry["sha256"],
                "artifactId": entry["artifactId"],
                "artifactName": entry["artifactName"],
            }
            for entry in manifest["targets"].values()
        ],
    }


def make_evidence():
    return {
        "schemaVersion": 1,
        "repository": REPOSITORY,
        "version": VERSION,
        "commit": COMMIT,
        "candidate": {
            "workflowRunId": RUN_ID,
            "workflowRunAttempt": RUN_ATTEMPT,
            "metadataArtifactId": METADATA_ID,
            "candidateRecordArtifactId": RECORD_ID,
        },
        "platforms": [
            {"target": target, "verdict": "passed", "evidence": EVIDENCE_LINK}
            for target in candidate_manifest.TARGETS
        ],
        "checklist": [
            {"id": item, "verdict": "passed", "evidence": EVIDENCE_LINK}
            for item in CHECKLIST
        ],
        "confirmation": confirmation(),
    }


def write_fixture(root):
    manifest = make_manifest()
    record = make_record(manifest)
    evidence = make_evidence()
    manifest_path = os.path.join(root, "moor-release-manifest-v1.json")
    sums_path = os.path.join(root, "SHA256SUMS")
    record_path = os.path.join(root, "moor-release-candidate-record.json")
    evidence_path = os.path.join(root, "manual-qa-evidence.txt")
    qa_path = os.path.join(root, "moor-release-qa-v1.json")
    write_bytes(manifest_path, canonical(manifest))
    write_bytes(
        sums_path,
        "".join(
            f'{entry["sha256"]}  {entry["asset"]}\n'
            for entry in manifest["targets"].values()
        ).encode("ascii"),
    )
    write_bytes(record_path, canonical(record))
    write_bytes(evidence_path, canonical(evidence))
    return {
        "manifest": manifest,
        "record": record,
        "evidence": evidence,
        "manifest_path": manifest_path,
        "sums_path": sums_path,
        "record_path": record_path,
        "evidence_path": evidence_path,
        "qa_path": qa_path,
    }


def command(fixture, verb):
    args = [
        sys.executable,
        TOOL,
        verb,
        "--manifest",
        fixture["manifest_path"],
        "--sums",
        fixture["sums_path"],
        "--candidate-record",
        fixture["record_path"],
        "--metadata-artifact-id",
        METADATA_ID,
        "--candidate-record-artifact-id",
        RECORD_ID,
        "--evidence-file",
        fixture["evidence_path"],
        "--evidence-url",
        EVIDENCE_URL,
        "--evidence-comment-id",
        EVIDENCE_COMMENT_ID,
        "--evidence-author",
        APPROVED_BY,
        "--evidence-author-association",
        "OWNER",
        "--evidence-created-at",
        EVIDENCE_TIME,
        "--evidence-updated-at",
        EVIDENCE_TIME,
    ]
    if verb == "create":
        args += ["--out", fixture["qa_path"]]
    else:
        args += ["--qa-record", fixture["qa_path"]]
    return args


def run(fixture, verb="create", **replacements):
    args = command(fixture, verb)
    for flag, value in replacements.items():
        option = "--" + flag.replace("_", "-")
        args[args.index(option) + 1] = value
    return subprocess.run(args, capture_output=True, text=True)


def rewrite(path, mutate):
    with open(path, encoding="ascii") as handle:
        value = json.load(handle)
    mutate(value)
    write_bytes(path, canonical(value))


def expect_reject(label, mutator=None, **replacements):
    with tempfile.TemporaryDirectory(prefix="release-qa-reject-") as root:
        fixture = write_fixture(root)
        if mutator:
            mutator(fixture)
        result = run(fixture, **replacements)
        assert result.returncode != 0, f"{label}: unexpectedly accepted"
        assert result.stderr.strip(), f"{label}: no diagnostic"


def main():
    with tempfile.TemporaryDirectory(prefix="release-qa-valid-") as root:
        fixture = write_fixture(root)
        result = run(fixture)
        assert result.returncode == 0, result.stderr
        with open(fixture["qa_path"], "rb") as handle:
            first = handle.read()
        assert first.endswith(b"\n") and not first.endswith(b"\n\n") and b"\r" not in first
        result = run(fixture)
        assert result.returncode == 0, result.stderr
        with open(fixture["qa_path"], "rb") as handle:
            assert handle.read() == first, "same inputs did not produce identical QA bytes"
        result = run(fixture, verb="verify")
        assert result.returncode == 0, result.stderr
        qa = json.loads(first)
        assert list(qa) == [
            "schemaVersion",
            "repository",
            "version",
            "commit",
            "candidate",
            "coverage",
            "targets",
            "manualQa",
        ]
        assert qa["candidate"]["metadataArtifactId"] == METADATA_ID
        assert qa["candidate"]["candidateRecordArtifactId"] == RECORD_ID
        assert qa["candidate"]["manifestSha256"] == hashlib.sha256(
            open(fixture["manifest_path"], "rb").read()
        ).hexdigest()
        assert qa["candidate"]["sha256sumsSha256"] == hashlib.sha256(
            open(fixture["sums_path"], "rb").read()
        ).hexdigest()
        assert list(qa["targets"]) == candidate_manifest.TARGETS
        assert [qa["targets"][target]["manualQa"]["verdict"] for target in candidate_manifest.TARGETS] == [
            "passed"
        ] * 4
        assert [item["id"] for item in qa["manualQa"]["checklist"]] == CHECKLIST
        assert qa["manualQa"]["approvedBy"] == APPROVED_BY
        assert qa["manualQa"]["approvedAt"] == EVIDENCE_TIME
        assert qa["manualQa"]["confirmation"] == confirmation()
        assert qa["manualQa"]["evidence"]["sha256"] == hashlib.sha256(
            open(fixture["evidence_path"], "rb").read()
        ).hexdigest()

        tampered = copy.deepcopy(qa)
        first_target = candidate_manifest.TARGETS[0]
        tampered["targets"][first_target]["sha256"] = "f" * 64
        write_bytes(fixture["qa_path"], canonical(tampered))
        result = run(fixture, verb="verify")
        assert result.returncode != 0 and result.stderr.strip(), "tampered QA record passed verify"

    expect_reject(
        "candidate-record commit mismatch",
        lambda f: rewrite(f["record_path"], lambda value: value.update(commit="b" * 40)),
    )
    expect_reject(
        "candidate-record target digest mismatch",
        lambda f: rewrite(f["record_path"], lambda value: value["targets"][0].update(sha256="f" * 64)),
    )
    expect_reject(
        "checksum bytes mismatch",
        lambda f: open(f["sums_path"], "ab").write(b"extra\n"),
    )
    expect_reject(
        "evidence commit mismatch",
        lambda f: rewrite(f["evidence_path"], lambda value: value.update(commit="b" * 40)),
    )
    expect_reject(
        "platform failed",
        lambda f: rewrite(
            f["evidence_path"], lambda value: value["platforms"][0].update(verdict="failed")
        ),
    )
    expect_reject(
        "platform evidence is not a hosted run",
        lambda f: rewrite(
            f["evidence_path"],
            lambda value: value["platforms"][0].update(
                evidence="https://github.com/BrainyBlaze/desk/issues/37"
            ),
        ),
    )
    expect_reject(
        "platform missing",
        lambda f: rewrite(f["evidence_path"], lambda value: value["platforms"].pop()),
    )
    expect_reject(
        "platform reordered",
        lambda f: rewrite(f["evidence_path"], lambda value: value["platforms"].reverse()),
    )
    expect_reject(
        "checklist failed",
        lambda f: rewrite(
            f["evidence_path"], lambda value: value["checklist"][0].update(verdict="failed")
        ),
    )
    expect_reject(
        "checklist evidence is not a hosted run",
        lambda f: rewrite(
            f["evidence_path"],
            lambda value: value["checklist"][0].update(
                evidence="https://github.com/BrainyBlaze/moor/pull/1"
            ),
        ),
    )
    expect_reject(
        "checklist missing",
        lambda f: rewrite(f["evidence_path"], lambda value: value["checklist"].pop()),
    )
    expect_reject(
        "checklist reordered",
        lambda f: rewrite(f["evidence_path"], lambda value: value["checklist"].reverse()),
    )
    expect_reject(
        "wrong confirmation",
        lambda f: rewrite(f["evidence_path"], lambda value: value.update(confirmation="APPROVE")),
    )
    expect_reject("foreign evidence URL", evidence_url="https://example.com/qa")
    expect_reject("non-owner evidence", evidence_author_association="MEMBER")
    expect_reject("edited evidence", evidence_updated_at="2026-08-17T09:31:00Z")
    expect_reject("wrong metadata artifact input", metadata_artifact_id="999")
    expect_reject("wrong record artifact input", candidate_record_artifact_id="999")

    print("release QA record tests: deterministic acceptance and 18 rejection cases passed")


if __name__ == "__main__":
    main()
