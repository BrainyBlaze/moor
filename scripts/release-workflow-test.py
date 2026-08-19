#!/usr/bin/env python3
"""Static permission, ordering, and zero-build contract for release workflows."""

import os
import re
from pathlib import Path

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DESK_RELEASE_QA_COMMIT = "14e727bafe11a41e87a81a068c3ecbd3151fd2c8"


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
    candidate = read(".github/workflows/release-candidate.yml")
    candidate_qa = read(".github/workflows/release-candidate-qa.yml")
    qa = read(".github/workflows/release-qa.yml")
    promotion = read(".github/workflows/release-promote.yml")
    quality = read(".github/workflows/quality.yml")
    guide = read("docs/release-manual-qa-v1.md")
    manifest_contract = read("docs/release-manifest-v1.md")

    workflow_paths = sorted(
        list((Path(ROOT) / ".github" / "workflows").glob("*.yml"))
        + list((Path(ROOT) / ".github" / "workflows").glob("*.yaml"))
    )
    assert workflow_paths, "release governance: no workflow files found"
    for workflow_path in workflow_paths:
        forbid(
            workflow_path.read_text(encoding="utf-8"),
            "immutable-releases",
            f"workflow-token administration boundary ({workflow_path.name})",
        )

    assert candidate.count("github.run_attempt == 1") == 5, (
        "candidate immutable producer attempt: every job must reject reruns"
    )

    require(candidate_qa, "workflow_dispatch:", "candidate QA workflow")
    for name in (
        "candidate_run_id",
        "candidate_run_attempt",
        "metadata_artifact_id",
        "candidate_record_artifact_id",
    ):
        require(candidate_qa, name, "candidate QA inputs")
    forbid(candidate_qa, "inputs.desk_commit", "candidate QA protected Desk pin")
    forbid(candidate_qa, "desk_commit:", "candidate QA dispatch inputs")
    assert candidate_qa.count(f"DESK_COMMIT: {DESK_RELEASE_QA_COMMIT}") == 1, (
        "candidate QA must define the exact reviewed Desk commit once in protected workflow bytes"
    )
    require(candidate_qa, "actions: read", "candidate QA permissions")
    require(candidate_qa, "contents: read", "candidate QA permissions")
    forbid(candidate_qa, "contents: write", "candidate QA permissions")
    require(candidate_qa, "environment: release", "candidate QA environment")
    require(candidate_qa, "github.ref == 'refs/heads/main'", "candidate QA main gate")
    require(candidate_qa, "github.ref_protected", "candidate QA protected-ref gate")
    for runner in ("ubuntu-22.04", "ubuntu-24.04-arm", "macos-15-intel", "macos-15"):
        require(candidate_qa, runner, "candidate QA four-host matrix")
    require(candidate_qa, "actions/artifacts/$METADATA_ID", "candidate QA metadata by ID")
    require(candidate_qa, "actions/artifacts/$RECORD_ID", "candidate QA record by ID")
    require(candidate_qa, "actions/artifacts/$ARTIFACT_ID", "candidate QA binary by ID")
    require(candidate_qa, "repository: BrainyBlaze/desk", "candidate QA Desk checkout")
    require(candidate_qa, "ref: ${{ env.DESK_COMMIT }}", "candidate QA exact Desk commit")
    require(candidate_qa, "git -C desk rev-parse HEAD", "candidate QA checked-out Desk identity")
    require(candidate_qa, "scripts/project-moor-pin.mjs", "candidate QA pin projection")
    require(candidate_qa, "DESK_MOOR_RELEASE_BASE_URL", "candidate QA local candidate origin")
    require(candidate_qa, "npm run fetch:moor", "candidate QA installer path")
    require(candidate_qa, "DESK_MOOR_NATIVE_BIN", "candidate QA exact holder path")
    forbid(candidate_qa, "cache: npm", "candidate QA untrusted Desk cache boundary")
    forbid(candidate_qa, '"moor 0.1.0"', "candidate QA manifest-derived version")
    for suite in (
        "tests/moor-native-e2e.test.ts",
        "tests/restore-attach-retention.test.ts",
        "tests/controller-link-recovery.test.ts",
        "tests/terminalDaemon.test.ts",
        "tests/terminalDaemonMain.test.ts",
    ):
        require(candidate_qa, suite, "candidate QA integration suite")
    require(candidate_qa, "manual-qa-evidence.json", "candidate QA evidence artifact")
    require(candidate_qa, "moor-release-candidate-qa-evidence", "candidate QA evidence artifact")
    forbid(candidate_qa, 'handle.write("\\n")', "candidate QA comment-stable evidence bytes")
    require(candidate_qa, "github.run_attempt == 1", "candidate QA immutable attempt")
    require(candidate_qa, 'path == ".github/workflows/release-candidate.yml"', "candidate QA source run")
    for command in ("npm run build:moor", "cargo build", "cargo install", "cargo package"):
        forbid(candidate_qa, command, "candidate QA zero-build boundary")

    require(qa, "workflow_dispatch:", "QA workflow")
    for name in (
        "candidate_run_id",
        "candidate_run_attempt",
        "metadata_artifact_id",
        "candidate_record_artifact_id",
        "candidate_qa_run_id",
        "candidate_qa_run_attempt",
        "candidate_qa_evidence_artifact_id",
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
    require(
        qa,
        "if: github.ref == 'refs/heads/main' && github.ref_protected && github.run_attempt == 1",
        "QA immutable producer attempt",
    )
    require(qa, '--qa-run-id "${{ github.run_id }}"', "QA record producer run binding")
    require(qa, '--qa-run-attempt "${{ github.run_attempt }}"', "QA record producer attempt binding")
    require(qa, "actions/runs/$RUN_ID/attempts/$RUN_ATTEMPT", "QA run identity")
    require(qa, "actions/artifacts/$METADATA_ID", "QA metadata by ID")
    require(qa, "actions/artifacts/$RECORD_ID", "QA record by ID")
    require(qa, "actions/runs/$QA_RUN_ID/attempts/$QA_RUN_ATTEMPT", "QA candidate-QA run identity")
    require(qa, "actions/artifacts/$EVIDENCE_ARTIFACT_ID", "QA evidence by ID")
    require(qa, 'path == ".github/workflows/release-candidate-qa.yml"', "QA candidate-QA workflow")
    forbid(qa, "moor-0.1.0-linux-x64", "QA manifest-derived binary smoke")
    require(qa, "moor-release-candidate-qa-evidence", "QA evidence artifact name")
    require(qa, "candidate-qa-evidence/manual-qa-evidence.json", "QA evidence artifact bytes")
    require(qa, "--candidate-qa-run-id", "QA record candidate-QA binding")
    require(qa, "--candidate-qa-evidence-artifact-id", "QA record evidence binding")
    require(qa, "--desk-commit", "QA record Desk binding")
    require(qa, "collaborators/$EVIDENCE_AUTHOR/permission", "QA admin permission proof")
    require(qa, 'permission == "admin"', "QA admin permission proof")
    require(qa, "release-qa-record.py create", "QA record construction")
    require(qa, "moor-release-qa-v1", "QA artifact")
    forbid(qa, "gh release", "QA mutation boundary")
    forbid(qa, "git/refs", "QA mutation boundary")
    for command in ("cargo build", "cargo install", "cargo package"):
        forbid(qa, command, "QA zero-build boundary")

    require(promotion, "workflow_dispatch:", "promotion workflow")
    for name in (
        "mode",
        "qa_run_id",
        "qa_run_attempt",
        "qa_artifact_id",
        "promotion_issue_number",
        "promotion_nonce",
        "original_promote_run_id",
        "preflight_comment_id",
        "completion_comment_id",
    ):
        require(promotion, name, "promotion inputs")
    require(promotion, "actions: read", "promotion permissions")
    require(promotion, "contents: read", "promotion permissions")
    require(promotion, "issues: read", "promotion permissions")
    forbid(promotion, "contents: write", "promotion permissions")
    forbid(promotion, "issues: write", "promotion permissions")
    for malformed in ("${ github.", "${ inputs.", "${ steps."):
        forbid(promotion, malformed, "malformed GitHub expression")
    forbid(promotion, "attestation_issue_number", "retired attestation input")
    forbid(promotion, "attestation_nonce", "retired attestation input")
    forbid(promotion, "release-admin-attestation.py", "retired attestation protocol")
    require(promotion, "environment: release", "promotion environment")
    require(promotion, "github.ref == 'refs/heads/main'", "promotion main gate")
    require(promotion, "github.ref_protected", "promotion protected-ref gate")
    require(promotion, "attempt_one:", "promotion immutable attempt prerequisite")
    require(promotion, 'test "${{ github.run_attempt }}" = 1', "promotion immutable attempt prerequisite")
    require(promotion, "needs: attempt_one", "promotion immutable attempt prerequisite")
    promote_job = promotion[promotion.index("  promote:") :]
    ordered(
        promote_job,
        [
            "Require attempt-one execution inside the promotion job",
            "Check out the reviewed promotion tooling from protected main",
        ],
        "promotion failed-job rerun gate",
    )
    require(promotion, "release_promotion_record.py manifest create", "canonical promotion manifest")
    require(promotion, "promotion-manifest.json", "canonical promotion manifest")
    require(promotion, "moor-release-promotion-v1", "promotion bundle")
    require(promotion, "actions/upload-artifact@", "promotion bundle")
    require(promotion, "artifact-digest", "promotion bundle digest")
    require(promotion, ".digest == $digest", "promotion REST digest comparison")
    require(promotion, "Open the local-administrator promotion gate", "promotion gate-ready boundary")
    require(promotion, "GATE_READY_AT", "promotion gate-ready boundary")
    require(promotion, "GITHUB_STEP_SUMMARY", "promotion gate-ready disclosure")
    require(promotion, "python3 scripts/release-admin-promote.py promote", "complete local command")
    for option in (
        "--repository",
        "--promotion-run-id",
        "--promotion-run-attempt",
        "--head-sha",
        "--source-artifact-id",
        "--source-artifact-name",
        "--source-api-digest",
        "--issue-number",
        "--dispatcher",
        "--gate-ready-at",
        "--nonce",
        "--transaction-root",
    ):
        require(promotion, option, "complete local command")
    require(promotion, "Wait for local administrator preflight", "named preflight wait")
    require(promotion, "Wait for local administrator completion", "named completion wait")
    require(promotion, "for attempt in $(seq 1 180)", "bounded preflight wait")
    require(promotion, "for attempt in $(seq 1 360)", "bounded completion wait")
    require(promotion, "sleep 10", "bounded local-administrator wait")
    require(promotion, "preflight wait timed out", "promotion named timeout")
    require(promotion, "completion wait timed out", "promotion named timeout")
    require(promotion, "release_promotion_record.py preflight verify", "promotion preflight verifier")
    require(promotion, "release_promotion_record.py completion verify", "promotion completion verifier")
    assert promotion.count(".authority.immutableReleaseSettings.responseBase64") == 4, (
        "promotion must verify the exact settings-response bytes from both records "
        "in normal and recovery modes"
    )
    forbid(
        promotion,
        "printf '%s' '{\"enabled\":true,\"enforced_by_owner\":false}'",
        "promotion must not synthesize settings-response evidence",
    )
    require(promotion, "collaborators/$COMMENT_AUTHOR/permission", "promotion live admin proof")
    require(promotion, 'permission == "admin"', "promotion live admin proof")
    require(promotion, ".candidateQa.workflowRunId", "promotion candidate-QA run binding")
    require(promotion, ".candidateQa.evidenceArtifactId", "promotion evidence artifact binding")
    require(promotion, 'path == ".github/workflows/release-candidate-qa.yml"', "promotion candidate-QA workflow")
    require(promotion, "moor-release-candidate-qa-evidence", "promotion evidence artifact name")
    require(promotion, "candidate-qa-evidence/manual-qa-evidence.json", "promotion evidence artifact bytes")
    require(promotion, ".qaRun.workflowRunId == $run", "promotion QA producer run binding")
    require(promotion, ".qaRun.workflowRunAttempt == $attempt", "promotion QA producer attempt binding")
    require(promotion, ".qaRun.workflowRunId", "promotion QA record reconstruction")
    require(promotion, "release-qa-record.py verify", "promotion QA verification")
    require(promotion, "release-asset-transaction.py verify-complete", "promotion asset verifier")
    require(promotion, "refs/tags/$VERSION", "exact tag")
    require(promotion, ".draft == false", "published release proof")
    require(promotion, ".immutable == true", "immutable release proof")
    require(promotion, "Independently verify the immutable published release", "independent proof")
    require(promotion, "final-completion-comment.json", "same completion final refetch")
    require(
        promotion,
        "cmp completion-comment.json final-completion-comment.json",
        "same completion final refetch",
    )
    require(promotion, 'test "$(jq length expected-assets.json)" = 6', "exact six-asset publication")
    require(promotion, "inputs.mode == 'promote'", "promote-only human gates")
    require(promotion, "inputs.mode == 'verify-published'", "read-only recovery mode")
    require(promotion, "original_promote_run_id", "read-only recovery source")
    require(promotion, "completion_comment_id", "read-only recovery receipt")
    forbid(promotion, "--clobber", "no overwrite")
    for command in (
        "cargo build",
        "cargo install",
        "cargo package",
        "candidate-manifest.py",
        "gh release upload",
        "--method POST",
        "--method PATCH",
        "--method DELETE",
        "uploads.github.com",
    ):
        forbid(promotion, command, "promotion zero-build boundary")
    ordered(
        promotion,
        [
            "release-qa-record.py verify",
            "Assemble the six byte-identical release files and expected inventory",
            "release_promotion_record.py manifest create",
            "actions/upload-artifact@",
            "Open the local-administrator promotion gate",
            "Wait for local administrator preflight",
            "Wait for local administrator completion",
            "Independently verify the immutable published release",
        ],
        "promotion workflow",
    )

    for test in (
        "python3 scripts/release-qa-record-test.py",
        "python3 scripts/release-asset-transaction-test.py",
        "python3 scripts/release-promotion-record-test.py",
        "python3 scripts/release-admin-promote-test.py",
        "python3 scripts/release-workflow-test.py",
    ):
        require(quality, test, "hosted quality")

    pinned_actions(candidate_qa, "candidate QA workflow")
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
    require(guide, "pin schema 3", "Desk pin schema")
    forbid(guide, "schema-2 pin", "retired Desk pin schema")
    require(guide, "never rebuilds or overwrites", "promotion boundary")
    require(guide, "single permitted deletion", "promotion starter exception")
    require(guide, "authenticated local `gh` session", "promotion local authority")
    require(guide, "sole release mutator", "promotion local authority")
    require(guide, "one complete, pasteable command", "promotion operator surface")
    require(guide, "canonical preflight and completion", "promotion record protocol")
    require(guide, "fresh nonce", "promotion record freshness")
    require(guide, "exact response bytes", "promotion settings authority")
    require(guide, "X-GitHub-Api-Version: 2026-03-10", "promotion pinned settings API")
    require(guide, "application/vnd.github+json", "promotion pinned settings media type")
    require(
        guide,
        '{"enabled":true,"enforced_by_owner":false}',
        "promotion live settings response shape",
    )
    require(guide, "persistent but not immutable", "promotion comment evidence boundary")
    require(guide, "never rerun a failed promotion attempt", "promotion attempt recovery")
    require(guide, "`verify-published`", "read-only promotion recovery")
    require(guide, "preflight-to-publication", "promotion trusted-admin race")
    require(
        guide,
        "detection, not prepublication fail-closed prevention",
        "postpublication detection semantics",
    )
    require(manifest_contract, "moor-release-qa-v1.json", "manifest QA record contract")
    require(manifest_contract, "candidate-QA evidence artifact", "manifest candidate-QA contract")
    require(manifest_contract, "QA producer run/attempt", "manifest QA producer identity")
    require(manifest_contract, "repository `admin` permission", "manifest evidence authority")
    require(manifest_contract, "read-only promotion workflow", "manifest workflow authority")
    require(manifest_contract, "canonical preflight and completion records", "manifest record protocol")
    require(manifest_contract, "exact response bytes", "manifest promotion settings authority")
    require(manifest_contract, "new attempt-1 dispatch", "manifest promotion recovery")
    require(manifest_contract, "`starter`", "manifest starter exception")
    require(manifest_contract, "pin schema version is `3`", "manifest Desk pin schema")
    forbid(manifest_contract, "pin schema version is `2`", "retired manifest Desk pin schema")
    print("release workflow safety contract: OK")


if __name__ == "__main__":
    main()
