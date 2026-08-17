#!/usr/bin/env python3
"""Crash/resume and conflict tests for the draft release asset planner."""

import hashlib
import json
import os
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
TOOL = os.path.join(HERE, "release-asset-transaction.py")
FILES = {
    "moor-0.1.0-linux-x64": b"linux candidate\n",
    "SHA256SUMS": b"sums\n",
}


def expected_entries():
    return [
        {"name": name, "size": len(body), "sha256": hashlib.sha256(body).hexdigest()}
        for name, body in FILES.items()
    ]


def asset(asset_id, name, state="uploaded", size=None):
    if size is None:
        size = len(FILES.get(name, b"foreign\n"))
    return {"id": asset_id, "name": name, "state": state, "size": size}


def invoke(
    command,
    assets,
    downloads=None,
    expected=None,
    release_draft=True,
    starter_deletions=None,
):
    downloads = downloads or {}
    expected = expected if expected is not None else expected_entries()
    with tempfile.TemporaryDirectory(prefix="release-assets-") as root:
        release_files = os.path.join(root, "release-files")
        boundary = os.path.join(root, "downloads")
        os.mkdir(release_files)
        os.mkdir(boundary)
        for name, body in FILES.items():
            with open(os.path.join(release_files, name), "wb") as handle:
                handle.write(body)
        expected_path = os.path.join(root, "expected.json")
        with open(expected_path, "w", encoding="ascii", newline="") as handle:
            json.dump(expected, handle)
            handle.write("\n")
        assets_path = os.path.join(root, "assets.json")
        with open(assets_path, "w", encoding="ascii", newline="") as handle:
            json.dump(assets, handle)
            handle.write("\n")
        starter_deletions_path = os.path.join(root, "starter-deletions.json")
        with open(starter_deletions_path, "w", encoding="ascii", newline="") as handle:
            json.dump(starter_deletions or {}, handle)
            handle.write("\n")
        for asset_id, body in downloads.items():
            with open(os.path.join(boundary, str(asset_id)), "wb") as handle:
                handle.write(body)
        return subprocess.run(
            [
                sys.executable,
                TOOL,
                command,
                "--release-files",
                release_files,
                "--expected",
                expected_path,
                "--assets",
                assets_path,
                "--downloads",
                boundary,
                "--release-draft",
                "true" if release_draft else "false",
                "--starter-deletions",
                starter_deletions_path,
            ],
            capture_output=True,
            text=True,
        )


def plan(assets, downloads=None, release_draft=True, starter_deletions=None):
    result = invoke(
        "plan",
        assets,
        downloads,
        release_draft=release_draft,
        starter_deletions=starter_deletions,
    )
    assert result.returncode == 0, result.stderr
    return json.loads(result.stdout)


def reject(
    label,
    assets,
    downloads=None,
    expected=None,
    command="plan",
    release_draft=True,
    starter_deletions=None,
):
    result = invoke(
        command,
        assets,
        downloads,
        expected,
        release_draft,
        starter_deletions,
    )
    assert result.returncode != 0, f"{label}: unexpectedly accepted"
    assert result.stderr.strip(), f"{label}: no diagnostic"


def main():
    first, second = FILES
    assert plan([]) == {"complete": False, "action": {"kind": "upload", "name": first}}
    assert plan([asset(11, first)], {11: FILES[first]}) == {
        "complete": False,
        "action": {"kind": "upload", "name": second},
    }
    complete_assets = [asset(11, first), asset(12, second)]
    complete_downloads = {11: FILES[first], 12: FILES[second]}
    assert plan(complete_assets, complete_downloads) == {"complete": True}
    verified = invoke("verify-complete", complete_assets, complete_downloads)
    assert verified.returncode == 0, verified.stderr
    assert json.loads(verified.stdout) == {"complete": True}

    assert plan([asset(19, first, "starter", size=999)]) == {
        "complete": False,
        "action": {"kind": "delete-starter", "name": first, "id": 19},
    }

    reject("wrong existing bytes", [asset(20, first)], {20: b"wrong\n"})
    reject("missing existing download", [asset(21, first)])
    reject("unexpected asset", [asset(22, "foreign")], {22: b"foreign\n"})
    reject(
        "duplicate asset name",
        [asset(23, first), asset(24, first)],
        {23: FILES[first], 24: FILES[first]},
    )
    reject(
        "duplicate asset id",
        [asset(25, first), asset(25, second)],
        {25: FILES[first]},
    )
    reject("unexpected starter asset", [asset(26, "foreign", "starter", size=999)])
    reject(
        "starter asset on published release",
        [asset(27, first, "starter", size=999)],
        release_draft=False,
    )
    reject(
        "third starter deletion exceeds the per-run bound",
        [asset(28, first, "starter", size=999)],
        starter_deletions={first: 2},
    )
    reject(
        "starter asset is never complete",
        [asset(29, first, "starter", size=999), asset(30, second)],
        {30: FILES[second]},
        command="verify-complete",
    )
    reject(
        "expected metadata lies about local bytes",
        [],
        expected=[dict(expected_entries()[0], size=999), expected_entries()[1]],
    )
    reject(
        "duplicate expected name",
        [],
        expected=[expected_entries()[0], expected_entries()[0]],
    )
    reject(
        "partial inventory is not complete",
        [asset(31, first)],
        {31: FILES[first]},
        command="verify-complete",
    )

    print("release asset transaction tests: 5 resume states and 12 rejection cases passed")


if __name__ == "__main__":
    main()
