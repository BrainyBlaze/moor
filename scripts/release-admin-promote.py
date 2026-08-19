#!/usr/bin/env python3
"""Perform Moor release mutations from one authenticated local administrator session."""

import argparse
import hashlib
import importlib.util
import json
import os
import stat
import subprocess
import sys
import tempfile
import zipfile
from datetime import datetime, timezone
from email.utils import parsedate_to_datetime
from types import SimpleNamespace
from urllib.parse import quote

import release_promotion_record as records


HERE = os.path.dirname(os.path.abspath(__file__))
ASSET_PLANNER_PATH = os.path.join(HERE, "release-asset-transaction.py")
_planner_spec = importlib.util.spec_from_file_location(
    "release_admin_asset_planner", ASSET_PLANNER_PATH
)
asset_planner = importlib.util.module_from_spec(_planner_spec)
_planner_spec.loader.exec_module(asset_planner)


class Refusal(Exception):
    """The transaction cannot safely proceed from the observed state."""


class AmbiguousMutation(Exception):
    """A mutation request has no trustworthy response and must be observed."""


def refuse(message):
    raise Refusal(message)


class GhClient:
    """Minimal GitHub REST adapter with evidence-first, single-attempt writes."""

    def __init__(self, transaction_root, delivery_root, *, runner=None):
        self.transaction_root = transaction_root
        self.delivery_root = delivery_root
        self.runner = runner or self._run_command
        self.sequence = 0
        self.frozen = False
        self._authenticated_user = None
        self._authenticated_scopes = None

    @staticmethod
    def _run_command(command, input_bytes):
        return subprocess.run(
            command,
            input=input_bytes,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )

    @staticmethod
    def _split_response(output):
        remaining = output
        while remaining.startswith(b"HTTP/"):
            split = None
            for separator in (b"\r\n\r\n", b"\n\n"):
                if separator in remaining:
                    split = remaining.split(separator, 1)
                    break
            if split is None:
                return None, {}, output
            header_body, body = split
            lines = header_body.replace(b"\r\n", b"\n").split(b"\n")
            try:
                status = int(lines[0].split()[1])
            except (IndexError, ValueError):
                return None, {}, output
            headers = {}
            for line in lines[1:]:
                if b":" not in line:
                    continue
                name, value = line.split(b":", 1)
                try:
                    headers[name.decode("ascii").lower()] = value.strip().decode(
                        "ascii"
                    )
                except UnicodeDecodeError:
                    refuse("GitHub response contains a non-ASCII header")
            if not body.startswith(b"HTTP/"):
                return status, headers, body
            remaining = body
        return None, {}, output

    def _evidence_root(self):
        root = self.delivery_root if self.frozen else self.transaction_root
        os.makedirs(root, exist_ok=True)
        return root

    def _record_exchange(
        self, method, path, request_body, result, status, headers, response_body
    ):
        self.sequence += 1
        prefix = f"api-{self.sequence:04d}"
        root = self._evidence_root()
        request_path = os.path.join(root, f"{prefix}-request.json")
        request_body_path = os.path.join(root, f"{prefix}-request-body.bin")
        response_path = os.path.join(root, f"{prefix}-response.json")
        body_path = os.path.join(root, f"{prefix}-response-body.bin")
        request_record = {
            "method": method,
            "path": path,
            "bodyBytes": 0 if request_body is None else len(request_body),
            "bodySha256": None
            if request_body is None
            else hashlib.sha256(request_body).hexdigest(),
        }
        response_record = {
            "exitCode": result.returncode,
            "status": status,
            "headers": {
                name: headers[name]
                for name in ("date", "x-github-request-id", "x-oauth-scopes")
                if name in headers
            },
            "bodyBytes": len(response_body),
            "bodySha256": hashlib.sha256(response_body).hexdigest(),
            "stderr": result.stderr.decode("utf-8", "replace")[:4096],
        }
        _write_canonical_json(request_path, request_record)
        if request_body is not None:
            with open(request_body_path, "wb") as handle:
                handle.write(request_body)
        _write_canonical_json(response_path, response_record)
        with open(body_path, "wb") as handle:
            handle.write(response_body)

    def _api(
        self,
        method,
        path,
        *,
        body=None,
        accept=None,
        content_type=None,
        allowed=(200,),
    ):
        command = [
            "gh",
            "api",
            "--include",
            "-H",
            "X-GitHub-Api-Version: 2026-03-10",
        ]
        if accept is not None:
            command.extend(["-H", f"Accept: {accept}"])
        if content_type is not None:
            command.extend(["-H", f"Content-Type: {content_type}"])
        if body is not None:
            command.extend(["--method", method, "--input", "-"])
        elif method != "GET":
            command.extend(["--method", method])
        command.append(path)
        result = self.runner(command, body)
        status, headers, response_body = self._split_response(result.stdout)
        self._record_exchange(
            method, path, body, result, status, headers, response_body
        )
        if status in allowed:
            return status, headers, response_body
        if status is None and method in ("POST", "PATCH", "DELETE"):
            raise AmbiguousMutation(f"{method} {path} returned no HTTP response")
        if method in ("POST", "PATCH", "DELETE") and status in (408, 429, 500, 502, 503, 504):
            raise AmbiguousMutation(f"{method} {path} returned HTTP {status}")
        if status is None:
            refuse(f"GET {path} returned no HTTP response")
        refuse(f"GitHub API {method} {path} returned HTTP {status}")

    def _json(self, method, path, *, payload=None, allowed=(200,)):
        body = None if payload is None else records.canonical_json(payload)
        status, headers, response = self._api(
            method,
            path,
            body=body,
            accept="application/vnd.github+json",
            allowed=allowed,
        )
        if status == 204:
            return None, headers
        try:
            return json.loads(response.decode("utf-8")), headers
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            if method in ("POST", "PATCH", "DELETE"):
                raise AmbiguousMutation(
                    f"{method} {path} returned an unreadable success response"
                ) from error
            refuse(f"GitHub API {method} {path} returned invalid JSON: {error}")

    def _pages(self, path):
        values = []
        for page in range(1, 1001):
            separator = "&" if "?" in path else "?"
            value, _headers = self._json(
                "GET", f"{path}{separator}per_page=100&page={page}"
            )
            if not isinstance(value, list):
                refuse(f"paginated GitHub response for {path} is not an array")
            values.extend(value)
            if len(value) < 100:
                return values
        refuse(f"pagination for {path} exceeded 1000 pages")

    @staticmethod
    def _normalize_release(value):
        if not isinstance(value, dict):
            refuse("release response is not an object")
        return {
            "id": value.get("id"),
            "html_url": value.get("html_url"),
            "tag_name": value.get("tag_name"),
            "target_commitish": value.get("target_commitish"),
            "name": value.get("name"),
            "body": value.get("body"),
            "draft": value.get("draft"),
            "prerelease": value.get("prerelease"),
            "immutable": value.get("immutable"),
        }

    @staticmethod
    def _normalize_asset(value):
        if not isinstance(value, dict):
            refuse("release asset response is not an object")
        return {
            "id": value.get("id"),
            "name": value.get("name"),
            "state": value.get("state"),
            "size": value.get("size"),
        }

    @staticmethod
    def _utc_now():
        return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")

    def _load_authenticated_user(self):
        if self._authenticated_user is not None:
            return
        value, headers = self._json("GET", "user")
        login = value.get("login") if isinstance(value, dict) else None
        if not isinstance(login, str) or not login:
            refuse("authenticated GitHub user response has no login")
        scope_header = headers.get("x-oauth-scopes")
        if not isinstance(scope_header, str):
            refuse("authenticated GitHub response has no OAuth scope header")
        self._authenticated_user = login
        self._authenticated_scopes = {
            scope.strip() for scope in scope_header.split(",") if scope.strip()
        }

    def authenticated_login(self):
        self._load_authenticated_user()
        return self._authenticated_user

    def oauth_scopes(self):
        self._load_authenticated_user()
        return set(self._authenticated_scopes)

    def repository_permission(self, repository, actor):
        value, _headers = self._json(
            "GET", f"repos/{repository}/collaborators/{quote(actor, safe='')}/permission"
        )
        if not isinstance(value, dict):
            refuse("repository permission response is not an object")
        return value.get("permission")

    def github_time_and_settings(self, repository):
        _status, headers, body = self._api(
            "GET",
            f"repos/{repository}/immutable-releases",
            accept="application/vnd.github+json",
        )
        date_header = headers.get("date")
        try:
            server_time = parsedate_to_datetime(date_header).astimezone(timezone.utc)
        except (TypeError, ValueError) as error:
            refuse(f"GitHub response Date header is invalid: {error}")
        return (
            server_time.strftime("%Y-%m-%dT%H:%M:%SZ"),
            self._utc_now(),
            body,
        )

    def current_time(self):
        return self._utc_now()

    def local_time(self):
        return self._utc_now()

    def get_workflow_run(self, repository, run_id, run_attempt):
        value, _headers = self._json(
            "GET", f"repos/{repository}/actions/runs/{run_id}/attempts/{run_attempt}"
        )
        return value

    def get_artifact_metadata(self, repository, artifact_id):
        value, _headers = self._json(
            "GET", f"repos/{repository}/actions/artifacts/{artifact_id}"
        )
        return value

    def download_artifact(self, repository, artifact_id, output_path):
        _status, _headers, body = self._api(
            "GET",
            f"repos/{repository}/actions/artifacts/{artifact_id}/zip",
            accept="application/octet-stream",
        )
        with open(output_path, "wb") as handle:
            handle.write(body)

    def workflow_step(self, repository, run_id, run_attempt, step_name):
        jobs = self._pages(
            f"repos/{repository}/actions/runs/{run_id}/attempts/{run_attempt}/jobs"
        )
        matches = []
        for job in jobs:
            steps = job.get("steps") if isinstance(job, dict) else None
            if not isinstance(steps, list):
                continue
            matches.extend(step for step in steps if step.get("name") == step_name)
        if len(matches) > 1:
            refuse(f"multiple workflow steps are named {step_name!r}")
        return None if not matches else matches[0].get("status")

    def require_live_step(self, repository, run_id, run_attempt, step_name):
        if self.workflow_step(repository, run_id, run_attempt, step_name) != "in_progress":
            refuse(f"workflow step {step_name!r} is not in progress")

    def get_tag(self, repository, version):
        status, _headers, body = self._api(
            "GET",
            f"repos/{repository}/git/ref/tags/{quote(version, safe='')}",
            accept="application/vnd.github+json",
            allowed=(200, 404),
        )
        if status == 404:
            return None
        try:
            value = json.loads(body.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError) as error:
            refuse(f"tag response is invalid JSON: {error}")
        obj = value.get("object") if isinstance(value, dict) else None
        if not isinstance(obj, dict) or obj.get("type") != "commit":
            refuse("release tag is not a lightweight commit ref")
        return {"ref": value.get("ref"), "sha": obj.get("sha")}

    def create_tag(self, repository, version, commit):
        value, _headers = self._json(
            "POST",
            f"repos/{repository}/git/refs",
            payload={"ref": f"refs/tags/{version}", "sha": commit},
            allowed=(201,),
        )
        obj = value.get("object") if isinstance(value, dict) else None
        return {
            "ref": value.get("ref") if isinstance(value, dict) else None,
            "sha": obj.get("sha") if isinstance(obj, dict) else None,
        }

    def list_releases(self, repository, version):
        return [
            self._normalize_release(value)
            for value in self._pages(f"repos/{repository}/releases")
            if isinstance(value, dict) and value.get("tag_name") == version
        ]

    def create_release(self, repository, payload):
        value, _headers = self._json(
            "POST", f"repos/{repository}/releases", payload=payload, allowed=(201,)
        )
        return self._normalize_release(value)

    def get_release(self, repository, release_id):
        value, _headers = self._json(
            "GET", f"repos/{repository}/releases/{release_id}"
        )
        return self._normalize_release(value)

    def list_assets(self, repository, release_id):
        return [
            self._normalize_asset(value)
            for value in self._pages(f"repos/{repository}/releases/{release_id}/assets")
        ]

    def download_asset(self, repository, asset_id, output_path):
        _status, _headers, body = self._api(
            "GET",
            f"repos/{repository}/releases/assets/{asset_id}",
            accept="application/octet-stream",
        )
        with open(output_path, "wb") as handle:
            handle.write(body)

    def upload_asset(self, repository, release_id, name, path):
        with open(path, "rb") as handle:
            body = handle.read()
        self._api(
            "POST",
            f"https://uploads.github.com/repos/{repository}/releases/{release_id}/assets?name={quote(name, safe='')}",
            body=body,
            accept="application/vnd.github+json",
            content_type="application/octet-stream",
            allowed=(201,),
        )

    def get_asset(self, repository, asset_id):
        value, _headers = self._json(
            "GET", f"repos/{repository}/releases/assets/{asset_id}"
        )
        return self._normalize_asset(value)

    def delete_asset(self, repository, asset_id):
        self._api(
            "DELETE",
            f"repos/{repository}/releases/assets/{asset_id}",
            accept="application/vnd.github+json",
            allowed=(204,),
        )

    def publish_release(self, repository, release_id):
        value, _headers = self._json(
            "PATCH",
            f"repos/{repository}/releases/{release_id}",
            payload={"draft": False},
        )
        return self._normalize_release(value)

    def list_comments(self, repository, issue_number):
        return self._pages(f"repos/{repository}/issues/{issue_number}/comments")

    def post_comment(self, repository, issue_number, body):
        value, _headers = self._json(
            "POST",
            f"repos/{repository}/issues/{issue_number}/comments",
            payload={"body": body},
            allowed=(201,),
        )
        return value

    def freeze_evidence(self):
        self.frozen = True


def _timestamp(value, what):
    try:
        return records.timestamp(value, what)
    except records.Invalid as error:
        refuse(str(error))


def verify_premutation_authority(
    config,
    github,
    *,
    checkout_head,
    checkout_clean,
    local_time=None,
):
    """Verify every local and remote authority gate before any mutation."""

    if not checkout_clean:
        refuse("checkout is not clean")
    if checkout_head != config["head_sha"]:
        refuse("checkout HEAD differs from the protected-main promotion head")

    administrator = github.authenticated_login()
    if administrator != config["dispatcher"]:
        refuse("authenticated user differs from the promotion dispatcher")
    scopes = github.oauth_scopes()
    if not {"repo", "workflow"}.issubset(scopes):
        refuse("authenticated session lacks required repo and workflow OAuth scopes")
    if github.repository_permission(config["repository"], administrator) != "admin":
        refuse("authenticated user does not have repository admin permission")

    server_time, checked_at, settings_body = github.github_time_and_settings(
        config["repository"]
    )
    try:
        records.validate_settings_response(settings_body)
    except records.Invalid as error:
        refuse(str(error))
    server = _timestamp(server_time, "GitHub server time")
    checked = _timestamp(checked_at, "immutable release settings check time")
    gate_ready = _timestamp(config["gate_ready_at"], "gate-ready time")
    if abs(server - checked) > records.CLOCK_SKEW:
        refuse("GitHub server time and settings check differ beyond clock skew")
    if checked - gate_ready < -records.CLOCK_SKEW:
        refuse("immutable settings were checked before the gate was ready")
    if server - gate_ready < -records.CLOCK_SKEW:
        refuse("GitHub server time predates the gate beyond clock skew")
    if local_time is None:
        local = datetime.now(timezone.utc)
    else:
        local = _timestamp(local_time, "local UTC time")
    if abs(local - server) > records.CLOCK_SKEW:
        refuse("local UTC time and GitHub server time differ beyond clock skew")

    step_state = github.workflow_step(
        config["repository"],
        config["promotion_run_id"],
        config["promotion_run_attempt"],
        config["preflight_step"],
    )
    if step_state != "in_progress":
        refuse("promotion workflow is not waiting at the named preflight step")
    return {
        "administrator": administrator,
        "githubServerTime": server_time,
        "settingsCheckedAt": checked_at,
        "settingsResponse": settings_body,
    }


def _require_mutation_gate(context, github):
    github.require_live_step(
        context["repository"],
        context["promotion_run_id"],
        context["promotion_run_attempt"],
        context["completion_step"],
    )
    context["verify_preflight"]()


def _resolve_ambiguous(context, observe, validate, what):
    attempts = context.get("ambiguity_observations", 6)
    sleep = context.get("sleep")
    if sleep is None:
        import time

        sleep = time.sleep
    for attempt in range(attempts):
        observed = observe()
        if observed is not None:
            validate(observed, context)
            return observed
        if attempt + 1 < attempts:
            sleep(records.POLL_INTERVAL_SECONDS)
    refuse(f"ambiguous {what} did not resolve to exact state")


def _validate_tag(tag, context):
    expected = {
        "ref": f"refs/tags/{context['version']}",
        "sha": context["candidate_commit"],
    }
    if tag != expected:
        refuse("release tag conflicts with the approved candidate")
    return tag


def _validate_release(release, context):
    if not isinstance(release, dict):
        refuse("release response is not an object")
    expected = {
        "tag_name": context["version"],
        "target_commitish": context["candidate_commit"],
        "name": context["release_name"],
        "body": context["release_body"],
        "prerelease": False,
    }
    for key, value in expected.items():
        if release.get(key) != value:
            refuse(f"release {key} conflicts with the approved metadata")
    if release.get("draft") is True:
        if release.get("immutable") not in (False, None):
            refuse("draft release unexpectedly reports immutable state")
        return "draft"
    if release.get("draft") is False and release.get("immutable") is True:
        return "published"
    refuse("release is neither the exact draft nor an immutable publication")


def _resolve_release(context, github):
    matches = github.list_releases(context["repository"], context["version"])
    if len(matches) > 1:
        refuse("multiple releases use the approved tag")
    if not matches:
        return None
    release = matches[0]
    _validate_release(release, context)
    return release


def _write_json(path, value):
    with open(path, "w", encoding="ascii", newline="") as handle:
        json.dump(value, handle, separators=(",", ":"))
        handle.write("\n")


def _observe_assets(context, github, release, deletion_counts, verb):
    observation = tempfile.mkdtemp(
        prefix="asset-observation-", dir=context["transaction_root"]
    )
    downloads = os.path.join(observation, "downloads")
    os.mkdir(downloads)
    assets = github.list_assets(context["repository"], release["id"])
    inventory = [
        {
            "id": asset.get("id"),
            "name": asset.get("name"),
            "state": asset.get("state"),
            "size": asset.get("size"),
        }
        for asset in assets
    ]
    assets_path = os.path.join(observation, "assets.json")
    deletions_path = os.path.join(observation, "starter-deletions.json")
    _write_json(assets_path, inventory)
    _write_json(deletions_path, deletion_counts)
    for asset in inventory:
        if asset["state"] == "uploaded":
            github.download_asset(
                context["repository"],
                asset["id"],
                os.path.join(downloads, str(asset["id"])),
            )
    arguments = SimpleNamespace(
        verb=verb,
        release_files=context["release_files"],
        expected=context["expected_assets"],
        assets=assets_path,
        downloads=downloads,
        release_draft="true" if release["draft"] else "false",
        starter_deletions=deletions_path,
    )
    try:
        plan = asset_planner.evaluate(arguments)
    except asset_planner.Invalid as error:
        refuse(str(error))
    return plan, inventory, downloads


def _published_asset_projection(inventory, downloads):
    projection = []
    for asset in inventory:
        path = os.path.join(downloads, str(asset["id"]))
        digest = hashlib.sha256()
        with open(path, "rb") as handle:
            for chunk in iter(lambda: handle.read(1024 * 1024), b""):
                digest.update(chunk)
        projection.append(
            {
                "id": asset["id"],
                "name": asset["name"],
                "size": asset["size"],
                "sha256": digest.hexdigest(),
            }
        )
    return sorted(projection, key=lambda item: item["name"].encode("ascii"))


def _fresh_release_fence(context, github, release_id, require_draft=True):
    _validate_tag(github.get_tag(context["repository"], context["version"]), context)
    release = github.get_release(context["repository"], release_id)
    state = _validate_release(release, context)
    if require_draft and state != "draft":
        refuse("release stopped being the exact draft before mutation")
    return release, state


def _resolve_ambiguous_asset_action(
    context, github, release, deletion_counts, original_action
):
    attempts = context.get("ambiguity_observations", 6)
    sleep = context.get("sleep")
    if sleep is None:
        import time

        sleep = time.sleep
    for attempt in range(attempts):
        current_release, _ = _fresh_release_fence(
            context, github, release["id"], require_draft=True
        )
        plan, _inventory, _downloads = _observe_assets(
            context, github, current_release, deletion_counts, "plan"
        )
        if plan.get("complete"):
            return
        observed_action = plan.get("action")
        if observed_action != original_action:
            if (
                original_action["kind"] == "delete-starter"
                and observed_action.get("kind") == "delete-starter"
            ):
                refuse("ambiguous starter deletion resolved to a different starter")
            return
        if attempt + 1 < attempts:
            sleep(records.POLL_INTERVAL_SECONDS)
    refuse(f"ambiguous {original_action['kind']} did not resolve to exact state")


def _resolve_ambiguous_publish(context, github, release_id):
    attempts = context.get("ambiguity_observations", 6)
    sleep = context.get("sleep")
    if sleep is None:
        import time

        sleep = time.sleep
    for attempt in range(attempts):
        _validate_tag(github.get_tag(context["repository"], context["version"]), context)
        release = github.get_release(context["repository"], release_id)
        if _validate_release(release, context) == "published":
            return release
        if attempt + 1 < attempts:
            sleep(records.POLL_INTERVAL_SECONDS)
    refuse("ambiguous publication did not resolve to immutable state")


def execute_release_transaction(context, github):
    """Create or adopt exact release state, then publish at most once."""

    tag = github.get_tag(context["repository"], context["version"])
    if tag is None:
        _require_mutation_gate(context, github)
        try:
            github.create_tag(
                context["repository"], context["version"], context["candidate_commit"]
            )
            tag = github.get_tag(context["repository"], context["version"])
        except AmbiguousMutation:
            tag = _resolve_ambiguous(
                context,
                lambda: github.get_tag(context["repository"], context["version"]),
                _validate_tag,
                "tag creation",
            )
    _validate_tag(tag, context)

    release = _resolve_release(context, github)
    if release is None:
        _require_mutation_gate(context, github)
        _validate_tag(github.get_tag(context["repository"], context["version"]), context)
        try:
            github.create_release(
                context["repository"],
                {
                    "tag_name": context["version"],
                    "target_commitish": context["candidate_commit"],
                    "name": context["release_name"],
                    "body": context["release_body"],
                    "draft": True,
                    "prerelease": False,
                    "generate_release_notes": False,
                },
            )
        except AmbiguousMutation:
            _resolve_ambiguous(
                context,
                lambda: _resolve_release(context, github),
                _validate_release,
                "draft release creation",
            )
        release = _resolve_release(context, github)
        if release is None:
            refuse("draft release creation did not resolve to exact state")

    state = _validate_release(release, context)
    deletion_counts = {}
    if state == "draft":
        for _iteration in range(50):
            release, state = _fresh_release_fence(
                context, github, release["id"], require_draft=True
            )
            plan, _inventory, _downloads = _observe_assets(
                context, github, release, deletion_counts, "plan"
            )
            if plan["complete"]:
                break
            action = plan["action"]
            _require_mutation_gate(context, github)
            release, _ = _fresh_release_fence(
                context, github, release["id"], require_draft=True
            )
            if action["kind"] == "upload":
                try:
                    github.upload_asset(
                        context["repository"],
                        release["id"],
                        action["name"],
                        os.path.join(context["release_files"], action["name"]),
                    )
                except AmbiguousMutation:
                    _resolve_ambiguous_asset_action(
                        context, github, release, deletion_counts, action
                    )
                continue
            if action["kind"] != "delete-starter":
                refuse("asset planner emitted an unsupported mutation")
            current = github.get_asset(context["repository"], action["id"])
            if current != {
                "id": action["id"],
                "name": action["name"],
                "state": "starter",
                "size": current.get("size"),
            }:
                refuse("starter asset changed before deletion")
            try:
                github.delete_asset(context["repository"], action["id"])
            except AmbiguousMutation:
                _resolve_ambiguous_asset_action(
                    context, github, release, deletion_counts, action
                )
            deletion_counts[action["name"]] = deletion_counts.get(action["name"], 0) + 1
        else:
            refuse("asset transaction did not converge within 50 planner decisions")

        _require_mutation_gate(context, github)
        authority = context["reauthorize"]("prepublish")
        release, _ = _fresh_release_fence(
            context, github, release["id"], require_draft=True
        )
        _observe_assets(context, github, release, deletion_counts, "verify-complete")
        try:
            github.publish_release(context["repository"], release["id"])
        except AmbiguousMutation:
            _resolve_ambiguous_publish(context, github, release["id"])
    else:
        authority = context["reauthorize"]("published-recovery")

    _validate_tag(github.get_tag(context["repository"], context["version"]), context)
    release = github.get_release(context["repository"], release["id"])
    if _validate_release(release, context) != "published":
        refuse("release did not become immutable after publication")
    plan, inventory, downloads = _observe_assets(
        context, github, release, deletion_counts, "verify-complete"
    )
    if plan != {"complete": True}:
        refuse("published asset verification did not complete")
    return {
        "authorityPhase": "prepublish" if state == "draft" else "published-recovery",
        "tag": tag,
        "release": release,
        "assets": _published_asset_projection(inventory, downloads),
        "starterDeletions": deletion_counts,
        "authority": authority,
    }


def _write_canonical_json(path, value):
    with open(path, "wb") as handle:
        handle.write(records.canonical_json(value))


def _sha256_file(path):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _validate_context_bindings(context):
    manifest, _body = records.load_promotion_manifest(context["promotion_manifest"])
    expected = {
        "repository": manifest["repository"],
        "promotion_run_id": manifest["promotion"]["workflowRunId"],
        "promotion_run_attempt": str(manifest["promotion"]["workflowRunAttempt"]),
        "head_sha": manifest["promotion"]["headSha"],
        "nonce": manifest["promotion"]["nonce"],
        "candidate_commit": manifest["candidate"]["commit"],
        "version": manifest["release"]["version"],
        "release_name": manifest["release"]["name"],
    }
    for key, wanted in expected.items():
        if context.get(key) != wanted:
            refuse(f"local promotion context {key} differs from the manifest")
    try:
        release_body = context["release_body"].encode("ascii")
    except (KeyError, AttributeError, UnicodeEncodeError):
        refuse("local release body is not ASCII text")
    if hashlib.sha256(release_body).hexdigest() != manifest["release"]["bodySha256"]:
        refuse("local release body differs from the manifest")
    try:
        with open(context["expected_assets"], encoding="ascii") as handle:
            expected_assets = json.load(handle)
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        refuse(f"cannot read expected assets: {error}")
    if expected_assets != manifest["assets"]:
        refuse("local expected assets differ from the manifest")
    return manifest


def _validate_source_run(config, run):
    expected = {
        "id": int(config["promotion_run_id"]),
        "run_attempt": 1,
        "event": "workflow_dispatch",
        "status": "in_progress",
        "conclusion": None,
        "head_branch": "main",
        "head_sha": config["head_sha"],
        "path": ".github/workflows/release-promote.yml",
    }
    if not isinstance(run, dict):
        refuse("promotion workflow run response is not an object")
    for key, wanted in expected.items():
        if run.get(key) != wanted:
            refuse(f"promotion workflow run {key} differs")
    actor = run.get("actor")
    if not isinstance(actor, dict) or actor.get("login") != config["dispatcher"]:
        refuse("promotion workflow dispatcher differs")


def _validate_source_artifact(config, artifact):
    expected = {
        "id": int(config["source_artifact_id"]),
        "name": config["source_artifact_name"],
        "expired": False,
        "digest": config["source_api_digest"],
    }
    if not isinstance(artifact, dict):
        refuse("promotion artifact response is not an object")
    for key, wanted in expected.items():
        if artifact.get(key) != wanted:
            refuse(f"promotion artifact {key} differs")
    producer = artifact.get("workflow_run")
    wanted_producer = {
        "id": int(config["promotion_run_id"]),
        "head_branch": "main",
        "head_sha": config["head_sha"],
    }
    if not isinstance(producer, dict):
        refuse("promotion artifact has no producer run")
    for key, wanted in wanted_producer.items():
        if producer.get(key) != wanted:
            refuse(f"promotion artifact producer {key} differs")


def _safe_bundle_members(archive):
    members = archive.infolist()
    names = [member.filename for member in members]
    if len(names) != len(set(names)):
        refuse("promotion bundle inventory contains duplicate paths")
    total = 0
    for member in members:
        try:
            member.filename.encode("ascii")
        except UnicodeEncodeError:
            refuse("promotion bundle inventory contains a non-ASCII path")
        parts = member.filename.split("/")
        if (
            member.is_dir()
            or member.flag_bits & 1
            or member.filename.startswith("/")
            or any(part in ("", ".", "..") for part in parts)
            or "\\" in member.filename
        ):
            refuse("promotion bundle inventory contains an unsafe path")
        mode = (member.external_attr >> 16) & 0o170000
        if mode == stat.S_IFLNK:
            refuse("promotion bundle inventory contains a symbolic link")
        if member.file_size > 512 * 1024 * 1024:
            refuse("promotion bundle member exceeds the size limit")
        total += member.file_size
    if total > 1024 * 1024 * 1024:
        refuse("promotion bundle exceeds the total size limit")
    return members


def _release_body_from_manifest(manifest):
    qa = manifest["qa"]["releaseQa"]
    candidate = manifest["candidate"]
    body = (
        f"Source-Commit: {candidate['commit']}\n"
        f"Candidate-Run: {candidate['workflowRunId']}/{candidate['workflowRunAttempt']}\n"
        f"Promotion-Transaction: {qa['workflowRunId']}/{qa['workflowRunAttempt']}/{qa['artifactId']}"
    )
    if hashlib.sha256(body.encode("ascii")).hexdigest() != manifest["release"]["bodySha256"]:
        refuse("deterministic release body differs from the promotion manifest")
    return body


def inspect_promotion_bundle(archive_path, bundle_root):
    """Extract and validate the closed seven-file promotion bundle."""

    if os.path.exists(bundle_root):
        refuse("promotion bundle extraction directory already exists")
    os.mkdir(bundle_root)
    try:
        with zipfile.ZipFile(archive_path) as archive:
            members = _safe_bundle_members(archive)
            names = [member.filename for member in members]
            if "promotion-manifest.json" not in names:
                refuse("promotion bundle inventory has no manifest")
            manifest_member = archive.getinfo("promotion-manifest.json")
            if manifest_member.file_size > 1024 * 1024:
                refuse("promotion manifest exceeds the size limit")
            manifest_path = os.path.join(bundle_root, "promotion-manifest.json")
            with archive.open(manifest_member) as source, open(manifest_path, "wb") as target:
                target.write(source.read())
            try:
                manifest, _body = records.load_promotion_manifest(manifest_path)
            except records.Invalid as error:
                refuse(str(error))
            expected_names = ["promotion-manifest.json"] + [
                f"release-files/{entry['name']}" for entry in manifest["assets"]
            ]
            if sorted(names, key=lambda item: item.encode("ascii")) != sorted(
                expected_names, key=lambda item: item.encode("ascii")
            ):
                refuse("promotion bundle inventory differs from the manifest")
            for member in members:
                if member.filename == "promotion-manifest.json":
                    continue
                output = os.path.join(bundle_root, *member.filename.split("/"))
                os.makedirs(os.path.dirname(output), exist_ok=True)
                with archive.open(member) as source, open(output, "wb") as target:
                    for chunk in iter(lambda: source.read(1024 * 1024), b""):
                        target.write(chunk)
    except zipfile.BadZipFile as error:
        refuse(f"promotion artifact is not a valid ZIP archive: {error}")

    release_files = os.path.join(bundle_root, "release-files")
    for expected in manifest["assets"]:
        path = os.path.join(release_files, expected["name"])
        try:
            size = os.path.getsize(path)
        except OSError as error:
            refuse(f"cannot inspect release file {expected['name']}: {error}")
        if size != expected["size"] or _sha256_file(path) != expected["sha256"]:
            refuse(f"release file {expected['name']} differs from the manifest")
    expected_assets = os.path.join(bundle_root, "expected-assets.json")
    _write_canonical_json(expected_assets, manifest["assets"])
    release_body_path = os.path.join(bundle_root, "release-body.txt")
    with open(release_body_path, "wb") as handle:
        handle.write(_release_body_from_manifest(manifest).encode("ascii"))
    return manifest, manifest_path, release_files, expected_assets


def prepare_run_bundle(config, github, *, helper_commit):
    """Authenticate and extract one closed in-progress promotion bundle."""

    try:
        records.repository(config["repository"], "repository")
        records.decimal(config["promotion_run_id"], "promotion run ID")
        records.decimal(config["source_artifact_id"], "promotion artifact ID")
        records.sha40(config["head_sha"], "promotion head SHA")
        records.nonce(config["nonce"], "promotion nonce")
        records.api_digest(config["source_api_digest"], "promotion artifact API digest")
        records.sha40(helper_commit, "helper commit")
    except (KeyError, records.Invalid) as error:
        refuse(str(error))
    if config["promotion_run_attempt"] != "1":
        refuse("promotion run attempt must be 1")
    if config["source_artifact_name"] != "moor-release-promotion-v1":
        refuse("promotion artifact name differs")
    if helper_commit != config["head_sha"]:
        refuse("helper commit differs from the promotion head")

    run = github.get_workflow_run(
        config["repository"],
        config["promotion_run_id"],
        config["promotion_run_attempt"],
    )
    _validate_source_run(config, run)
    artifact = github.get_artifact_metadata(
        config["repository"], config["source_artifact_id"]
    )
    _validate_source_artifact(config, artifact)
    archive_path = os.path.join(config["transaction_root"], "promotion-bundle.zip")
    github.download_artifact(
        config["repository"], config["source_artifact_id"], archive_path
    )

    bundle_root = os.path.join(config["transaction_root"], "promotion-bundle")
    manifest, manifest_path, release_files, expected_assets = inspect_promotion_bundle(
        archive_path, bundle_root
    )

    promotion = manifest["promotion"]
    bindings = {
        "repository": manifest["repository"],
        "promotion_run_id": promotion["workflowRunId"],
        "promotion_run_attempt": str(promotion["workflowRunAttempt"]),
        "head_sha": promotion["headSha"],
        "nonce": promotion["nonce"],
    }
    for key, actual in bindings.items():
        if config[key] != actual:
            refuse(f"promotion bundle {key} differs from the gate")

    candidate = manifest["candidate"]
    release_body = _release_body_from_manifest(manifest)
    context = dict(config)
    context.update(
        {
            "manifest": manifest,
            "promotion_manifest": manifest_path,
            "candidate_commit": candidate["commit"],
            "version": manifest["release"]["version"],
            "release_name": manifest["release"]["name"],
            "release_body": release_body,
            "release_files": release_files,
            "expected_assets": expected_assets,
            "preflight_step": "Wait for local administrator preflight",
            "completion_step": "Wait for local administrator completion",
            "helper_commit": helper_commit,
            "source": {
                "mode": "run-bundle",
                "workflowRunId": config["promotion_run_id"],
                "workflowRunAttempt": 1,
                "artifactId": int(config["source_artifact_id"]),
                "artifactName": config["source_artifact_name"],
                "apiDigest": config["source_api_digest"],
            },
        }
    )
    _validate_context_bindings(context)
    return context


def _record_arguments(context, authority, settings_path, **extra):
    source = context["source"]
    values = {
        "promotion_manifest": context["promotion_manifest"],
        "issue_number": context["issue_number"],
        "dispatcher": context["dispatcher"],
        "administrator": authority["administrator"],
        "gate_ready_at": context["gate_ready_at"],
        "github_server_time": authority["githubServerTime"],
        "settings_response": settings_path,
        "settings_checked_at": authority["settingsCheckedAt"],
        "helper_commit": context["helper_commit"],
        "source_mode": source["mode"],
        "source_run_id": None,
        "source_run_attempt": None,
        "source_artifact_id": None,
        "source_artifact_name": None,
        "source_api_digest": None,
    }
    if source["mode"] == "run-bundle":
        values.update(
            {
                "source_run_id": str(source["workflowRunId"]),
                "source_run_attempt": str(source["workflowRunAttempt"]),
                "source_artifact_id": str(source["artifactId"]),
                "source_artifact_name": source["artifactName"],
                "source_api_digest": source["apiDigest"],
            }
        )
    values.update(extra)
    return SimpleNamespace(**values)


def _verify_expected_comment(
    context,
    github,
    expected,
    marker,
    validator,
    what,
    snapshot_path,
    *,
    expected_comment_id=None,
    expected_body_sha256=None,
    final_recheck=False,
):
    _write_canonical_json(
        snapshot_path,
        github.list_comments(context["repository"], context["issue_number"]),
    )
    arguments = SimpleNamespace(
        comments=snapshot_path,
        expected_author=context["dispatcher"],
        now=github.current_time(),
        expected_comment_id=expected_comment_id,
        expected_body_sha256=expected_body_sha256,
        final_recheck=final_recheck,
    )
    try:
        return records.verify_comment(
            arguments,
            marker,
            expected["kind"],
            expected,
            validator,
            what,
        )
    except records.Waiting:
        return None
    except records.Invalid as error:
        refuse(str(error))


def _post_or_adopt_comment(
    context, github, expected, marker, validator, what, snapshot_path
):
    verified = _verify_expected_comment(
        context, github, expected, marker, validator, what, snapshot_path
    )
    if verified is not None:
        return verified
    body = records.encode_comment(marker, expected).decode("ascii")
    try:
        github.post_comment(context["repository"], context["issue_number"], body)
    except AmbiguousMutation:
        pass
    attempts = context.get("ambiguity_observations", 6)
    sleep = context.get("sleep")
    if sleep is None:
        import time

        sleep = time.sleep
    for attempt in range(attempts):
        verified = _verify_expected_comment(
            context, github, expected, marker, validator, what, snapshot_path
        )
        if verified is not None:
            return verified
        if attempt + 1 < attempts:
            sleep(records.POLL_INTERVAL_SECONDS)
    refuse(f"{what} comment mutation did not resolve to exact state")


def _wait_for_live_step(context, github, step_name):
    attempts = context.get("workflow_observations", 120)
    sleep = context.get("sleep")
    if sleep is None:
        import time

        sleep = time.sleep
    for attempt in range(attempts):
        state = github.workflow_step(
            context["repository"],
            context["promotion_run_id"],
            context["promotion_run_attempt"],
            step_name,
        )
        if state == "in_progress":
            return
        if state not in (None, "queued", "pending", "waiting"):
            refuse(f"workflow step {step_name!r} is no longer able to become live")
        if attempt + 1 < attempts:
            sleep(records.POLL_INTERVAL_SECONDS)
    refuse(f"workflow step {step_name!r} did not become live")


def run_promotion(context, github, *, checkout_head, checkout_clean):
    """Execute one manifest-bound local promotion and emit its canonical records."""

    _validate_context_bindings(context)
    checkout_state = context.get("checkout_state")
    if checkout_state is not None:
        checkout_head, checkout_clean = checkout_state()
    authority = verify_premutation_authority(
        context,
        github,
        checkout_head=checkout_head,
        checkout_clean=checkout_clean,
        local_time=github.local_time(),
    )
    preflight_settings = os.path.join(
        context["transaction_root"], "preflight-settings-response.json"
    )
    with open(preflight_settings, "wb") as handle:
        handle.write(authority["settingsResponse"])
    preflight_record = records.build_preflight(
        _record_arguments(context, authority, preflight_settings)
    )
    try:
        records.validate_preflight(preflight_record)
    except records.Invalid as error:
        refuse(str(error))
    preflight_snapshot = os.path.join(
        context["transaction_root"], "preflight-comments.json"
    )
    preflight = _post_or_adopt_comment(
        context,
        github,
        preflight_record,
        records.PREFLIGHT_MARKER,
        records.validate_preflight,
        "preflight",
        preflight_snapshot,
    )
    _wait_for_live_step(context, github, context["completion_step"])

    def verify_preflight():
        verified = _verify_expected_comment(
            context,
            github,
            preflight_record,
            records.PREFLIGHT_MARKER,
            records.validate_preflight,
            "preflight",
            preflight_snapshot,
            expected_comment_id=preflight["commentId"],
            expected_body_sha256=preflight["bodySha256"],
            final_recheck=True,
        )
        if verified is None:
            refuse("accepted preflight comment disappeared")
        return verified

    def reauthorize(phase):
        recheck = dict(context)
        recheck["preflight_step"] = context["completion_step"]
        current_head, current_clean = checkout_head, checkout_clean
        if checkout_state is not None:
            current_head, current_clean = checkout_state()
        current = verify_premutation_authority(
            recheck,
            github,
            checkout_head=current_head,
            checkout_clean=current_clean,
            local_time=github.local_time(),
        )
        current["phase"] = phase
        return current

    context["verify_preflight"] = verify_preflight
    context["reauthorize"] = reauthorize
    transaction = execute_release_transaction(context, github)

    completion_authority = transaction["authority"]
    completion_settings = os.path.join(
        context["transaction_root"], "completion-settings-response.json"
    )
    with open(completion_settings, "wb") as handle:
        handle.write(completion_authority["settingsResponse"])
    published_assets = os.path.join(
        context["transaction_root"], "published-assets.json"
    )
    _write_canonical_json(published_assets, transaction["assets"])
    evidence_path = os.path.join(
        context["transaction_root"], "transaction-evidence-manifest.json"
    )
    records.create_evidence_manifest(context["transaction_root"], evidence_path)

    tag = transaction["tag"]
    release = transaction["release"]
    completion_record = records.build_completion(
        _record_arguments(
            context,
            completion_authority,
            completion_settings,
            authority_phase=transaction["authorityPhase"],
            preflight_comment_id=preflight["commentId"],
            preflight_comment_url=preflight["commentUrl"],
            preflight_body_sha256=preflight["bodySha256"],
            tag_ref=tag["ref"],
            tag_sha=tag["sha"],
            release_id=str(release["id"]),
            release_url=release["html_url"],
            release_tag=release["tag_name"],
            release_name=release["name"],
            release_body_sha256=hashlib.sha256(
                release["body"].encode("ascii")
            ).hexdigest(),
            release_draft="true" if release["draft"] else "false",
            release_immutable="true" if release["immutable"] else "false",
            published_assets=published_assets,
            transaction_evidence_manifest_sha256=_sha256_file(evidence_path),
        )
    )
    try:
        records.validate_completion(completion_record)
    except records.Invalid as error:
        refuse(str(error))

    github.freeze_evidence()
    delivery_root = context.get("delivery_root")
    temporary_delivery = delivery_root is None
    if temporary_delivery:
        delivery_root = tempfile.mkdtemp(prefix="moor-completion-delivery-")
    else:
        os.makedirs(delivery_root, exist_ok=True)
    completion_snapshot = os.path.join(delivery_root, "completion-comments.json")
    try:
        completion = _post_or_adopt_comment(
            context,
            github,
            completion_record,
            records.COMPLETION_MARKER,
            records.validate_completion,
            "completion",
            completion_snapshot,
        )
        completion = _verify_expected_comment(
            context,
            github,
            completion_record,
            records.COMPLETION_MARKER,
            records.validate_completion,
            "completion",
            completion_snapshot,
            expected_comment_id=completion["commentId"],
            expected_body_sha256=completion["bodySha256"],
            final_recheck=True,
        )
        if completion is None:
            refuse("accepted completion comment disappeared")
    finally:
        if temporary_delivery:
            import shutil

            shutil.rmtree(delivery_root)
    return {"preflight": preflight, "transaction": transaction, "completion": completion}


def _git_checkout_state():
    head = subprocess.run(
        ["git", "rev-parse", "HEAD"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    status = subprocess.run(
        ["git", "status", "--porcelain", "--untracked-files=normal"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if head.returncode != 0 or status.returncode != 0:
        refuse("cannot inspect the local Git checkout")
    try:
        head_text = head.stdout.decode("ascii").strip()
    except UnicodeDecodeError:
        refuse("local Git HEAD is not ASCII")
    try:
        records.sha40(head_text, "local Git HEAD")
    except records.Invalid as error:
        refuse(str(error))
    return head_text, status.stdout == b""


def build_parser():
    parser = argparse.ArgumentParser(
        description="Perform one manifest-bound Moor release promotion locally"
    )
    commands = parser.add_subparsers(dest="command", required=True)
    promote = commands.add_parser("promote")
    promote.add_argument("--repository", required=True)
    promote.add_argument("--promotion-run-id", required=True)
    promote.add_argument("--promotion-run-attempt", required=True)
    promote.add_argument("--head-sha", required=True)
    promote.add_argument("--source-artifact-id", required=True)
    promote.add_argument("--source-artifact-name", required=True)
    promote.add_argument("--source-api-digest", required=True)
    promote.add_argument("--issue-number", required=True)
    promote.add_argument("--dispatcher", required=True)
    promote.add_argument("--gate-ready-at", required=True)
    promote.add_argument("--nonce", required=True)
    promote.add_argument("--transaction-root", required=True)
    promote.add_argument("--delivery-root")
    inspect = commands.add_parser("inspect-bundle")
    inspect.add_argument("--archive", required=True)
    inspect.add_argument("--out", required=True)
    return parser


def promote_from_arguments(args):
    checkout_head, checkout_clean = _git_checkout_state()
    if checkout_head != args.head_sha:
        refuse("local Git HEAD differs from the promotion head")
    if not checkout_clean:
        refuse("local Git checkout is not clean")
    transaction_root = os.path.abspath(os.path.expanduser(args.transaction_root))
    delivery_root = (
        os.path.abspath(os.path.expanduser(args.delivery_root))
        if args.delivery_root is not None
        else transaction_root + "-delivery"
    )
    for path, what in (
        (transaction_root, "transaction root"),
        (delivery_root, "delivery root"),
    ):
        if os.path.exists(path):
            refuse(f"{what} already exists")
    os.makedirs(os.path.dirname(transaction_root), exist_ok=True)
    os.mkdir(transaction_root)
    config = {
        "repository": args.repository,
        "promotion_run_id": args.promotion_run_id,
        "promotion_run_attempt": args.promotion_run_attempt,
        "head_sha": args.head_sha,
        "source_artifact_id": args.source_artifact_id,
        "source_artifact_name": args.source_artifact_name,
        "source_api_digest": args.source_api_digest,
        "issue_number": args.issue_number,
        "dispatcher": args.dispatcher,
        "gate_ready_at": args.gate_ready_at,
        "nonce": args.nonce,
        "transaction_root": transaction_root,
        "delivery_root": delivery_root,
    }
    github = GhClient(transaction_root, delivery_root)
    context = prepare_run_bundle(config, github, helper_commit=checkout_head)
    context["checkout_state"] = _git_checkout_state
    result = run_promotion(
        context,
        github,
        checkout_head=checkout_head,
        checkout_clean=checkout_clean,
    )
    print(f"Published immutable release: {result['transaction']['release']['html_url']}")
    print(
        "Transaction evidence: "
        + os.path.join(transaction_root, "transaction-evidence-manifest.json")
    )
    print(f"Completion delivery evidence: {delivery_root}")
    return result


def main():
    args = build_parser().parse_args()
    try:
        if args.command == "promote":
            promote_from_arguments(args)
            return
        if args.command == "inspect-bundle":
            inspect_promotion_bundle(
                os.path.abspath(args.archive), os.path.abspath(args.out)
            )
            print(f"Verified promotion bundle: {os.path.abspath(args.out)}")
            return
        refuse("unsupported command")
    except (Refusal, AmbiguousMutation, records.Invalid, OSError) as error:
        print(f"release-admin-promote: {error}", file=sys.stderr)
        raise SystemExit(1)


if __name__ == "__main__":
    main()
