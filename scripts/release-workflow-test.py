#!/usr/bin/env python3
"""Static permission, ordering, and zero-build contract for release workflows."""

import os
import re

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def read(relative):
    with open(os.path.join(ROOT, relative), encoding="utf-8") as handle:
        return handle.read()


def require(text, fragment, what):
    assert fragment in text, f"{what}: missing {fragment!r}"


def forbid(text, fragment, what):
    assert fragment not in text, f"{what}: forbidden {fragment!r}"


def ordered(text, fragments, what):
    positions = []
    for fragment in fragments:
        require(text, fragment, what)
        positions.append(text.index(fragment))
    assert positions == sorted(positions), f"{what}: unsafe order for {fragments!r}"


def pinned_actions(text, what):
    actions = re.findall(r"^\s*uses:\s*([^\s#]+)", text, flags=re.MULTILINE)
    assert actions, f"{what}: no actions"
    for action in actions:
        assert re.fullmatch(r"[^@]+@[0-9a-f]{40}", action), (
            f"{what}: action is not pinned to a full commit: {action}"
        )


def main():
    qa = read(".github/workflows/release-qa.yml")
    promotion = read(".github/workflows/release-promote.yml")
    quality = read(".github/workflows/quality.yml")
    guide = read("docs/release-manual-qa-v1.md")
    manifest_contract = read("docs/release-manifest-v1.md")

    require(qa, "workflow_dispatch:", "QA workflow")
    for name in (
        "candidate_run_id",
        "candidate_run_attempt",
        "metadata_artifact_id",
        "candidate_record_artifact_id",
        "evidence_issue_number",
        "evidence_comment_id",
    ):
        require(qa, name, "QA inputs")
    require(qa, "actions: read", "QA permissions")
    require(qa, "contents: read", "QA permissions")
    require(qa, "issues: read", "QA permissions")
    forbid(qa, "contents: write", "QA permissions")
    require(qa, "environment: release", "QA environment")
    require(qa, "github.ref == 'refs/heads/main'", "QA main gate")
    require(qa, "github.ref_protected", "QA protected-ref gate")
    require(qa, "actions/runs/$RUN_ID/attempts/$RUN_ATTEMPT", "QA run identity")
    require(qa, "actions/artifacts/$METADATA_ID", "QA metadata by ID")
    require(qa, "actions/artifacts/$RECORD_ID", "QA record by ID")
    require(qa, "release-qa-record.py create", "QA record construction")
    require(qa, "moor-release-qa-v1", "QA artifact")
    forbid(qa, "gh release", "QA mutation boundary")
    forbid(qa, "git/refs", "QA mutation boundary")
    for command in ("cargo build", "cargo install", "cargo package"):
        forbid(qa, command, "QA zero-build boundary")

    require(promotion, "workflow_dispatch:", "promotion workflow")
    require(promotion, "qa_run_id", "promotion inputs")
    require(promotion, "qa_run_attempt", "promotion inputs")
    require(promotion, "qa_artifact_id", "promotion inputs")
    require(promotion, "actions: read", "promotion permissions")
    require(promotion, "contents: write", "promotion permissions")
    require(promotion, "issues: read", "promotion permissions")
    require(promotion, "environment: release", "promotion environment")
    require(promotion, "github.ref == 'refs/heads/main'", "promotion main gate")
    require(promotion, "github.ref_protected", "promotion protected-ref gate")
    require(promotion, "repos/${{ github.repository }}/immutable-releases", "immutable release gate")
    require(promotion, ".enabled == true", "immutable release gate")
    require(promotion, "release-qa-record.py verify", "promotion QA verification")
    require(promotion, "release-asset-transaction.py plan", "promotion asset planner")
    require(promotion, "release-asset-transaction.py verify-complete", "promotion asset verifier")
    require(promotion, "refs/tags/$VERSION", "exact tag")
    require(promotion, '"draft": true', "draft release")
    require(promotion, '"draft": false', "release publication")
    require(promotion, "re-download the published release assets", "post-publication verification")
    forbid(promotion, "--clobber", "no overwrite")
    forbid(promotion, "--method DELETE", "no delete")
    for command in ("cargo build", "cargo install", "cargo package", "candidate-manifest.py"):
        forbid(promotion, command, "promotion zero-build boundary")
    ordered(
        promotion,
        [
            "release-qa-record.py verify",
            "refs/tags/$VERSION",
            '"draft": true',
            "release-asset-transaction.py plan",
            '"draft": false',
            "re-download the published release assets",
        ],
        "promotion workflow",
    )
    publish_step = promotion[promotion.index("- name: Publish the complete draft exactly once") :]
    ordered(
        publish_step,
        [
            "prepublish-assets.json",
            "release-asset-transaction.py verify-complete",
            '{"draft": false}',
        ],
        "promotion prepublish fence",
    )

    for test in (
        "python3 scripts/release-qa-record-test.py",
        "python3 scripts/release-asset-transaction-test.py",
        "python3 scripts/release-workflow-test.py",
    ):
        require(quality, test, "hosted quality")

    pinned_actions(qa, "QA workflow")
    pinned_actions(promotion, "promotion workflow")
    for target in (
        "x86_64-unknown-linux-musl",
        "aarch64-unknown-linux-musl",
        "x86_64-apple-darwin",
        "aarch64-apple-darwin",
    ):
        require(guide, target, "manual QA guide target matrix")
    for item in (
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
    ):
        require(guide, item, "manual QA guide checklist")
    require(guide, "four binaries plus `moor-release-manifest-v1.json` and `SHA256SUMS`", "release assets")
    require(guide, "hosted Actions run or job URL", "manual QA evidence boundary")
    require(guide, "never rebuilds, deletes, or overwrites", "promotion boundary")
    require(manifest_contract, "moor-release-qa-v1.json", "manifest QA record contract")
    print("release workflow safety contract: OK")


if __name__ == "__main__":
    main()
