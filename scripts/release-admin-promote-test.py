#!/usr/bin/env python3
"""Behavior tests for the local administrator release transaction."""

import importlib.util
import hashlib
import io
import json
import os
import tempfile
import zipfile
from types import SimpleNamespace


HERE = os.path.dirname(os.path.abspath(__file__))
TOOL = os.path.join(HERE, "release-admin-promote.py")


def load_tool():
    spec = importlib.util.spec_from_file_location("release_admin_promote", TOOL)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class FakeGitHub:
    def __init__(self, *, permission="admin"):
        self.permission = permission
        self.mutations = []

    def authenticated_login(self):
        return "levi770"

    def oauth_scopes(self):
        return {"repo", "workflow"}

    def repository_permission(self, repository, actor):
        assert repository == "BrainyBlaze/moor"
        assert actor == "levi770"
        return self.permission

    def github_time_and_settings(self, repository):
        assert repository == "BrainyBlaze/moor"
        return (
            "2026-08-19T12:30:01Z",
            "2026-08-19T12:30:02Z",
            b'{"enabled":true,"enforced_by_owner":false}',
        )

    def workflow_step(self, repository, run_id, run_attempt, step_name):
        assert repository == "BrainyBlaze/moor"
        assert run_id == "32030000000"
        assert run_attempt == "1"
        assert step_name == "Wait for local administrator preflight"
        return "in_progress"

    def post_comment(self, *_args, **_kwargs):
        self.mutations.append("post-comment")
        raise AssertionError("preflight refusal performed a mutation")


class FakeReleaseGitHub:
    def __init__(self):
        self.tag = None
        self.release = None
        self.assets = []
        self.asset_bytes = {}
        self.next_asset_id = 100
        self.mutations = []
        self.gate_reads = 0
        self.authority_phases = []

    def require_live_step(self, repository, run_id, run_attempt, step_name):
        assert (repository, run_id, run_attempt, step_name) == (
            "BrainyBlaze/moor",
            "32030000000",
            "1",
            "Wait for local administrator completion",
        )
        self.gate_reads += 1

    def verify_preflight_comment(self):
        self.gate_reads += 1

    def reauthorize(self, context, phase):
        assert context["repository"] == "BrainyBlaze/moor"
        self.authority_phases.append(phase)
        return {"phase": phase}

    def get_tag(self, repository, version):
        assert repository == "BrainyBlaze/moor"
        assert version == "v0.1.0"
        return self.tag

    def create_tag(self, repository, version, commit):
        self.mutations.append(("create-tag", version, commit))
        self.tag = {"ref": f"refs/tags/{version}", "sha": commit}
        return self.tag

    def list_releases(self, repository, version):
        assert repository == "BrainyBlaze/moor"
        if self.release is None or self.release["tag_name"] != version:
            return []
        return [dict(self.release)]

    def create_release(self, repository, payload):
        self.mutations.append(("create-release", payload))
        self.release = {
            "id": 77,
            "html_url": f"https://github.com/{repository}/releases/tag/{payload['tag_name']}",
            "tag_name": payload["tag_name"],
            "target_commitish": payload["target_commitish"],
            "name": payload["name"],
            "body": payload["body"],
            "draft": True,
            "prerelease": False,
            "immutable": False,
        }
        return dict(self.release)

    def get_release(self, repository, release_id):
        assert repository == "BrainyBlaze/moor"
        assert release_id == 77
        return dict(self.release)

    def list_assets(self, repository, release_id):
        assert repository == "BrainyBlaze/moor"
        assert release_id == 77
        return [dict(asset) for asset in self.assets]

    def download_asset(self, repository, asset_id, output_path):
        assert repository == "BrainyBlaze/moor"
        with open(output_path, "wb") as handle:
            handle.write(self.asset_bytes[asset_id])

    def upload_asset(self, repository, release_id, name, path):
        assert repository == "BrainyBlaze/moor"
        assert release_id == 77
        with open(path, "rb") as handle:
            body = handle.read()
        asset_id = self.next_asset_id
        self.next_asset_id += 1
        self.mutations.append(("upload", name))
        self.assets.append(
            {"id": asset_id, "name": name, "state": "uploaded", "size": len(body)}
        )
        self.asset_bytes[asset_id] = body

    def get_asset(self, repository, asset_id):
        assert repository == "BrainyBlaze/moor"
        return next(dict(asset) for asset in self.assets if asset["id"] == asset_id)

    def delete_asset(self, repository, asset_id):
        assert repository == "BrainyBlaze/moor"
        asset = next(asset for asset in self.assets if asset["id"] == asset_id)
        self.mutations.append(("delete-starter", asset["name"], asset_id))
        self.assets.remove(asset)

    def publish_release(self, repository, release_id):
        assert repository == "BrainyBlaze/moor"
        assert release_id == 77
        self.mutations.append(("publish", release_id))
        self.release["draft"] = False
        self.release["immutable"] = True
        return dict(self.release)


class AmbiguousTagGitHub(FakeReleaseGitHub):
    def __init__(self, tool):
        super().__init__()
        self.tool = tool

    def create_tag(self, repository, version, commit):
        super().create_tag(repository, version, commit)
        raise self.tool.AmbiguousMutation("tag create response was lost")


class AmbiguousOnceGitHub(FakeReleaseGitHub):
    def __init__(self, tool, operation):
        super().__init__()
        self.tool = tool
        self.operation = operation
        self.fired = False

    def _ambiguous(self, operation):
        if self.operation == operation and not self.fired:
            self.fired = True
            raise self.tool.AmbiguousMutation(f"{operation} response was lost")

    def create_release(self, repository, payload):
        result = super().create_release(repository, payload)
        self._ambiguous("create-release")
        return result

    def upload_asset(self, repository, release_id, name, path):
        super().upload_asset(repository, release_id, name, path)
        self._ambiguous("upload")

    def publish_release(self, repository, release_id):
        result = super().publish_release(repository, release_id)
        self._ambiguous("publish")
        return result


class FakePromotionGitHub(FakeReleaseGitHub):
    def __init__(self):
        super().__init__()
        self.comments = []
        self.next_comment_id = 500
        self.evidence_frozen = False

    def authenticated_login(self):
        return "levi770"

    def oauth_scopes(self):
        return {"repo", "workflow"}

    def repository_permission(self, repository, actor):
        assert repository == "BrainyBlaze/moor"
        assert actor == "levi770"
        return "admin"

    def github_time_and_settings(self, repository):
        assert repository == "BrainyBlaze/moor"
        return (
            "2026-08-19T12:30:01Z",
            "2026-08-19T12:30:02Z",
            b'{"enabled":true,"enforced_by_owner":false}',
        )

    def workflow_step(self, repository, run_id, run_attempt, step_name):
        assert (repository, run_id, run_attempt) == (
            "BrainyBlaze/moor",
            "32030000000",
            "1",
        )
        assert step_name in (
            "Wait for local administrator preflight",
            "Wait for local administrator completion",
        )
        return "in_progress"

    def list_comments(self, repository, issue_number):
        assert repository == "BrainyBlaze/moor"
        assert issue_number == "26"
        return [dict(comment) for comment in self.comments]

    def post_comment(self, repository, issue_number, body):
        assert not self.evidence_frozen or body.startswith(
            "<!-- moor-release-completion-v1 -->\n"
        )
        self.next_comment_id += 1
        comment_id = self.next_comment_id
        comment = {
            "id": comment_id,
            "body": body,
            "user": {"login": "levi770"},
            "issue_url": f"https://api.github.com/repos/{repository}/issues/{issue_number}",
            "html_url": f"https://github.com/{repository}/issues/{issue_number}#issuecomment-{comment_id}",
            "created_at": "2026-08-19T12:30:03Z",
            "updated_at": "2026-08-19T12:30:03Z",
        }
        self.comments.append(comment)
        kind = "completion-comment" if self.evidence_frozen else "preflight-comment"
        self.mutations.append((kind, comment_id))
        return dict(comment)

    def current_time(self):
        return "2026-08-19T12:30:04Z"

    def local_time(self):
        return "2026-08-19T12:30:01Z"

    def freeze_evidence(self):
        self.evidence_frozen = True


class FakeSourceGitHub:
    def __init__(self, archive, api_digest):
        self.archive = archive
        self.api_digest = api_digest

    def get_workflow_run(self, repository, run_id, run_attempt):
        assert (repository, run_id, run_attempt) == ("BrainyBlaze/moor", "32030000000", "1")
        return {
            "id": 32030000000,
            "run_attempt": 1,
            "event": "workflow_dispatch",
            "status": "in_progress",
            "conclusion": None,
            "head_branch": "main",
            "head_sha": "a" * 40,
            "path": ".github/workflows/release-promote.yml",
            "actor": {"login": "levi770"},
        }

    def get_artifact_metadata(self, repository, artifact_id):
        assert (repository, artifact_id) == ("BrainyBlaze/moor", "92")
        return {
            "id": 92,
            "name": "moor-release-promotion-v1",
            "expired": False,
            "digest": self.api_digest,
            "workflow_run": {
                "id": 32030000000,
                "head_branch": "main",
                "head_sha": "a" * 40,
            },
        }

    def download_artifact(self, repository, artifact_id, output_path):
        assert (repository, artifact_id) == ("BrainyBlaze/moor", "92")
        with open(output_path, "wb") as handle:
            handle.write(self.archive)


def preflight_config():
    return {
        "repository": "BrainyBlaze/moor",
        "dispatcher": "levi770",
        "promotion_run_id": "32030000000",
        "promotion_run_attempt": "1",
        "head_sha": "a" * 40,
        "gate_ready_at": "2026-08-19T12:30:00Z",
        "preflight_step": "Wait for local administrator preflight",
    }


def write_release_fixture(root):
    release_files = os.path.join(root, "release-files")
    os.mkdir(release_files)
    expected = []
    for index, name in enumerate(
        (
            "SHA256SUMS",
            "moor-0.1.0-linux-arm64",
            "moor-0.1.0-linux-x64",
            "moor-0.1.0-macos-arm64",
            "moor-0.1.0-macos-x64",
            "moor-release-manifest-v1.json",
        )
    ):
        body = f"asset-{index}\n".encode("ascii")
        with open(os.path.join(release_files, name), "wb") as handle:
            handle.write(body)
        expected.append(
            {"name": name, "size": len(body), "sha256": hashlib.sha256(body).hexdigest()}
        )
    expected_path = os.path.join(root, "expected-assets.json")
    with open(expected_path, "w", encoding="ascii", newline="") as handle:
        json.dump(expected, handle, separators=(",", ":"))
        handle.write("\n")
    context = {
        "repository": "BrainyBlaze/moor",
        "promotion_run_id": "32030000000",
        "promotion_run_attempt": "1",
        "completion_step": "Wait for local administrator completion",
        "version": "v0.1.0",
        "candidate_commit": "b" * 40,
        "release_name": "Moor v0.1.0",
        "release_body": "Source-Commit: "
        + "b" * 40
        + "\nCandidate-Run: 1/1\nPromotion-Transaction: 2/1/3",
        "release_files": release_files,
        "expected_assets": expected_path,
        "transaction_root": root,
        "ambiguity_observations": 1,
        "sleep": lambda _seconds: None,
    }
    return context


def write_promotion_fixture(tool, root):
    context = write_release_fixture(root)
    context["release_body"] = (
        "Source-Commit: "
        + "b" * 40
        + "\nCandidate-Run: 32003467728/1"
        + "\nPromotion-Transaction: 32016500124/1/91"
    )
    with open(context["expected_assets"], encoding="ascii") as handle:
        expected = json.load(handle)
    artifact_names = tool.records.EXPECTED_CANDIDATE_ARTIFACT_NAMES
    manifest = {
        "schemaVersion": 1,
        "kind": "moor-release-promotion-manifest-v1",
        "repository": "BrainyBlaze/moor",
        "promotion": {
            "workflowRunId": "32030000000",
            "workflowRunAttempt": 1,
            "headSha": "a" * 40,
            "mode": "promote",
            "nonce": "d" * 64,
        },
        "qa": {
            "candidateQa": {
                "workflowRunId": "32015916481",
                "workflowRunAttempt": 1,
                "artifactId": "90",
                "artifactName": "moor-release-candidate-qa-evidence",
                "apiDigest": "sha256:" + "9" * 64,
                "deskCommit": "e" * 40,
            },
            "releaseQa": {
                "workflowRunId": "32016500124",
                "workflowRunAttempt": 1,
                "artifactId": "91",
                "artifactName": "moor-release-qa-v1",
                "apiDigest": "sha256:" + "8" * 64,
            },
        },
        "candidate": {
            "workflowRunId": "32003467728",
            "workflowRunAttempt": 1,
            "commit": "b" * 40,
            "artifacts": [
                {
                    "id": str(index + 10),
                    "name": name,
                    "apiDigest": "sha256:" + f"{index + 1:x}" * 64,
                }
                for index, name in enumerate(artifact_names)
            ],
        },
        "release": {
            "version": "v0.1.0",
            "tag": "v0.1.0",
            "name": "Moor v0.1.0",
            "bodySha256": hashlib.sha256(context["release_body"].encode("ascii")).hexdigest(),
        },
        "assets": expected,
    }
    tool.records.validate_manifest(manifest)
    manifest_path = os.path.join(root, "promotion-manifest.json")
    with open(manifest_path, "wb") as handle:
        handle.write(tool.records.canonical_json(manifest))
    context.update(
        {
            "manifest": manifest,
            "promotion_manifest": manifest_path,
            "promotion_run_id": "32030000000",
            "promotion_run_attempt": "1",
            "head_sha": "a" * 40,
            "nonce": "d" * 64,
            "issue_number": "26",
            "dispatcher": "levi770",
            "gate_ready_at": "2026-08-19T12:30:00Z",
            "preflight_step": "Wait for local administrator preflight",
            "completion_step": "Wait for local administrator completion",
            "helper_commit": "c" * 40,
            "source": {
                "mode": "run-bundle",
                "workflowRunId": "32030000000",
                "workflowRunAttempt": 1,
                "artifactId": 92,
                "artifactName": "moor-release-promotion-v1",
                "apiDigest": "sha256:" + "7" * 64,
            },
        }
    )
    return context


def test_permission_refusal_is_premutation(tool):
    github = FakeGitHub(permission="write")
    try:
        tool.verify_premutation_authority(
            preflight_config(),
            github,
            checkout_head="a" * 40,
            checkout_clean=True,
            local_time="2026-08-19T12:30:01Z",
        )
    except tool.Refusal as error:
        assert str(error) == "authenticated user does not have repository admin permission"
    else:
        raise AssertionError("non-admin authority was accepted")
    assert github.mutations == []


def test_exact_release_transaction_uses_planner_and_publishes_once(tool):
    github = FakeReleaseGitHub()
    with tempfile.TemporaryDirectory(prefix="moor-admin-promote-") as root:
        context = write_release_fixture(root)
        context["verify_preflight"] = github.verify_preflight_comment
        context["reauthorize"] = lambda phase: github.reauthorize(context, phase)
        result = tool.execute_release_transaction(context, github)

    assert [mutation[0] for mutation in github.mutations] == [
        "create-tag",
        "create-release",
        "upload",
        "upload",
        "upload",
        "upload",
        "upload",
        "upload",
        "publish",
    ]
    assert github.gate_reads >= len(github.mutations) * 2
    assert github.authority_phases == ["prepublish"]
    assert result["release"]["draft"] is False
    assert result["release"]["immutable"] is True
    assert [asset["name"] for asset in result["assets"]] == sorted(
        asset["name"] for asset in github.assets
    )


def test_ambiguous_tag_creation_is_adopted_without_retry(tool):
    github = AmbiguousTagGitHub(tool)
    with tempfile.TemporaryDirectory(prefix="moor-admin-promote-") as root:
        context = write_release_fixture(root)
        context["verify_preflight"] = github.verify_preflight_comment
        context["reauthorize"] = lambda phase: github.reauthorize(context, phase)
        result = tool.execute_release_transaction(context, github)
    assert [mutation[0] for mutation in github.mutations].count("create-tag") == 1
    assert result["tag"] == {"ref": "refs/tags/v0.1.0", "sha": "b" * 40}


def test_other_ambiguous_mutations_are_observed_without_retry(tool):
    for operation in ("create-release", "upload", "publish"):
        github = AmbiguousOnceGitHub(tool, operation)
        with tempfile.TemporaryDirectory(prefix="moor-admin-promote-") as root:
            context = write_release_fixture(root)
            context["verify_preflight"] = github.verify_preflight_comment
            context["reauthorize"] = lambda phase: github.reauthorize(context, phase)
            result = tool.execute_release_transaction(context, github)
        assert github.fired, operation
        assert [mutation[0] for mutation in github.mutations].count("create-release") == 1
        assert len([mutation for mutation in github.mutations if mutation[0] == "upload"]) == 6
        assert [mutation[0] for mutation in github.mutations].count("publish") == 1
        assert result["release"]["immutable"] is True


def test_one_command_posts_canonical_preflight_and_completion(tool):
    github = FakePromotionGitHub()
    with tempfile.TemporaryDirectory(prefix="moor-admin-promote-") as root:
        context = write_promotion_fixture(tool, root)
        result = tool.run_promotion(
            context,
            github,
            checkout_head="a" * 40,
            checkout_clean=True,
        )
        evidence_path = os.path.join(root, "transaction-evidence-manifest.json")
        assert os.path.isfile(evidence_path)
    assert github.comments[0]["body"].startswith(tool.records.PREFLIGHT_MARKER.decode("ascii"))
    assert github.comments[1]["body"].startswith(tool.records.COMPLETION_MARKER.decode("ascii"))
    assert [mutation[0] for mutation in github.mutations][0] == "preflight-comment"
    assert [mutation[0] for mutation in github.mutations][-1] == "completion-comment"
    assert result["completion"]["record"]["release"]["immutable"] is True


def promotion_archive(context, extra=None):
    output = io.BytesIO()
    with zipfile.ZipFile(output, "w", compression=zipfile.ZIP_STORED) as archive:
        archive.write(context["promotion_manifest"], "promotion-manifest.json")
        for name in sorted(os.listdir(context["release_files"])):
            archive.write(
                os.path.join(context["release_files"], name),
                f"release-files/{name}",
            )
        if extra is not None:
            archive.writestr(extra, b"untrusted\n")
    return output.getvalue()


def source_config(root):
    return {
        "repository": "BrainyBlaze/moor",
        "promotion_run_id": "32030000000",
        "promotion_run_attempt": "1",
        "head_sha": "a" * 40,
        "source_artifact_id": "92",
        "source_artifact_name": "moor-release-promotion-v1",
        "source_api_digest": "sha256:" + "7" * 64,
        "issue_number": "26",
        "dispatcher": "levi770",
        "gate_ready_at": "2026-08-19T12:30:00Z",
        "nonce": "d" * 64,
        "transaction_root": root,
    }


def test_run_bundle_is_closed_and_manifest_derived(tool):
    with tempfile.TemporaryDirectory(prefix="moor-source-fixture-") as fixture_root:
        fixture = write_promotion_fixture(tool, fixture_root)
        archive = promotion_archive(fixture)
        with tempfile.TemporaryDirectory(prefix="moor-source-transaction-") as transaction:
            config = source_config(transaction)
            github = FakeSourceGitHub(archive, config["source_api_digest"])
            context = tool.prepare_run_bundle(config, github, helper_commit="a" * 40)
            assert context["candidate_commit"] == "b" * 40
            assert context["version"] == "v0.1.0"
            assert context["release_body"].startswith("Source-Commit: " + "b" * 40)
            assert sorted(os.listdir(context["release_files"])) == [
                entry["name"] for entry in fixture["manifest"]["assets"]
            ]


def test_inspect_bundle_derives_closed_verifier_inputs(tool):
    with tempfile.TemporaryDirectory(prefix="moor-inspect-fixture-") as fixture_root:
        fixture = write_promotion_fixture(tool, fixture_root)
        archive_body = promotion_archive(fixture)
        archive_path = os.path.join(fixture_root, "promotion.zip")
        with open(archive_path, "wb") as handle:
            handle.write(archive_body)
        output_root = os.path.join(fixture_root, "inspected")

        manifest, manifest_path, release_files, expected_assets = (
            tool.inspect_promotion_bundle(archive_path, output_root)
        )

        assert manifest == fixture["manifest"]
        assert manifest_path == os.path.join(output_root, "promotion-manifest.json")
        assert sorted(os.listdir(release_files)) == [
            entry["name"] for entry in fixture["manifest"]["assets"]
        ]
        with open(expected_assets, "rb") as handle:
            assert json.load(handle) == fixture["manifest"]["assets"]
        with open(os.path.join(output_root, "release-body.txt"), "rb") as handle:
            assert handle.read().startswith(b"Source-Commit: " + b"b" * 40)


def test_run_bundle_rejects_any_extra_zip_member(tool):
    with tempfile.TemporaryDirectory(prefix="moor-source-fixture-") as fixture_root:
        fixture = write_promotion_fixture(tool, fixture_root)
        archive = promotion_archive(fixture, extra="../escape")
        with tempfile.TemporaryDirectory(prefix="moor-source-transaction-") as transaction:
            config = source_config(transaction)
            github = FakeSourceGitHub(archive, config["source_api_digest"])
            try:
                tool.prepare_run_bundle(config, github, helper_commit="a" * 40)
            except tool.Refusal as error:
                assert "inventory" in str(error) or "unsafe" in str(error)
            else:
                raise AssertionError("unsafe bundle was accepted")


class FakeCommandRunner:
    def __init__(self, responses):
        self.responses = list(responses)
        self.calls = []

    def __call__(self, command, input_bytes):
        self.calls.append((list(command), input_bytes))
        return self.responses.pop(0)


def http_response(status, body, **headers):
    lines = [f"HTTP/2.0 {status} result"]
    lines.extend(f"{name}: {value}" for name, value in headers.items())
    return ("\r\n".join(lines) + "\r\n\r\n").encode("ascii") + body


def test_gh_client_records_routes_and_separates_delivery(tool):
    user = http_response(
        200,
        b'{"login":"levi770"}',
        date="Wed, 19 Aug 2026 12:30:01 GMT",
        **{"x-oauth-scopes": "repo, workflow"},
    )
    comment = http_response(
        201,
        b'{"id":501}',
        date="Wed, 19 Aug 2026 12:30:02 GMT",
    )
    runner = FakeCommandRunner(
        [
            SimpleNamespace(returncode=0, stdout=user, stderr=b""),
            SimpleNamespace(returncode=0, stdout=comment, stderr=b""),
        ]
    )
    with tempfile.TemporaryDirectory(prefix="moor-gh-client-") as root:
        transaction = os.path.join(root, "transaction")
        delivery = os.path.join(root, "delivery")
        os.mkdir(transaction)
        client = tool.GhClient(transaction, delivery, runner=runner)
        assert client.authenticated_login() == "levi770"
        assert client.oauth_scopes() == {"repo", "workflow"}
        client.freeze_evidence()
        assert client.post_comment("BrainyBlaze/moor", "26", "body\n") == {"id": 501}
        assert os.listdir(transaction)
        assert os.listdir(delivery)
        with open(os.path.join(delivery, "api-0002-request-body.bin"), "rb") as handle:
            assert handle.read() == runner.calls[1][1]
    assert any("issues/26/comments" in argument for argument in runner.calls[1][0])
    assert all("token" not in argument.lower() for call, _ in runner.calls for argument in call)


def test_gh_client_uses_final_redirect_response_and_binary_upload_type(tool):
    redirected = (
        b"HTTP/1.1 302 Found\r\nlocation: https://example.invalid/asset\r\n\r\n"
        b"HTTP/2 200 OK\r\ndate: Wed, 19 Aug 2026 12:30:01 GMT\r\n\r\npayload"
    )
    status, headers, body = tool.GhClient._split_response(redirected)
    assert status == 200
    assert headers["date"] == "Wed, 19 Aug 2026 12:30:01 GMT"
    assert body == b"payload"

    uploaded = http_response(201, b'{"id":701}')
    runner = FakeCommandRunner(
        [SimpleNamespace(returncode=0, stdout=uploaded, stderr=b"")]
    )
    with tempfile.TemporaryDirectory(prefix="moor-upload-client-") as root:
        transaction = os.path.join(root, "transaction")
        delivery = os.path.join(root, "delivery")
        os.mkdir(transaction)
        asset = os.path.join(root, "asset.bin")
        with open(asset, "wb") as handle:
            handle.write(b"asset-bytes")
        client = tool.GhClient(transaction, delivery, runner=runner)
        client.upload_asset("BrainyBlaze/moor", 41, "asset.bin", asset)
    assert "Content-Type: application/octet-stream" in runner.calls[0][0]


def test_gh_client_never_blindly_retries_an_ambiguous_write(tool):
    runner = FakeCommandRunner(
        [SimpleNamespace(returncode=1, stdout=b"", stderr=b"network closed")]
    )
    with tempfile.TemporaryDirectory(prefix="moor-gh-client-") as root:
        client = tool.GhClient(root, root + "-delivery", runner=runner)
        try:
            client.create_tag("BrainyBlaze/moor", "v0.1.0", "b" * 40)
        except tool.AmbiguousMutation:
            pass
        else:
            raise AssertionError("ambiguous write was treated as success")
    assert len(runner.calls) == 1


def test_cli_parser_exposes_one_complete_local_promote_command(tool):
    args = tool.build_parser().parse_args(
        [
            "promote",
            "--repository",
            "BrainyBlaze/moor",
            "--promotion-run-id",
            "32030000000",
            "--promotion-run-attempt",
            "1",
            "--head-sha",
            "a" * 40,
            "--source-artifact-id",
            "92",
            "--source-artifact-name",
            "moor-release-promotion-v1",
            "--source-api-digest",
            "sha256:" + "7" * 64,
            "--issue-number",
            "26",
            "--dispatcher",
            "levi770",
            "--gate-ready-at",
            "2026-08-19T12:30:00Z",
            "--nonce",
            "d" * 64,
            "--transaction-root",
            "/tmp/moor-promotion",
        ]
    )
    assert args.command == "promote"
    assert args.source_artifact_id == "92"
    inspect = tool.build_parser().parse_args(
        ["inspect-bundle", "--archive", "/tmp/bundle.zip", "--out", "/tmp/bundle"]
    )
    assert inspect.command == "inspect-bundle"


def main():
    tool = load_tool()
    test_permission_refusal_is_premutation(tool)
    test_exact_release_transaction_uses_planner_and_publishes_once(tool)
    test_ambiguous_tag_creation_is_adopted_without_retry(tool)
    test_other_ambiguous_mutations_are_observed_without_retry(tool)
    test_one_command_posts_canonical_preflight_and_completion(tool)
    test_run_bundle_is_closed_and_manifest_derived(tool)
    test_inspect_bundle_derives_closed_verifier_inputs(tool)
    test_run_bundle_rejects_any_extra_zip_member(tool)
    test_gh_client_records_routes_and_separates_delivery(tool)
    test_gh_client_uses_final_redirect_response_and_binary_upload_type(tool)
    test_gh_client_never_blindly_retries_an_ambiguous_write(tool)
    test_cli_parser_exposes_one_complete_local_promote_command(tool)
    print("release admin promotion tests: authority, exact publish, ambiguous adoption, and receipts passed")


if __name__ == "__main__":
    main()
