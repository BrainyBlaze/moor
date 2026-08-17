#!/usr/bin/env python3
"""Plan or verify a no-overwrite release asset transaction.

The only delete plan this tool can emit is for an expected-name GitHub asset
whose freshly observed state is ``starter`` on the transaction's draft. Such
an asset is an incomplete upload record, never accepted release bytes.
"""

import argparse
import hashlib
import json
import os
import re
import sys

HEX64 = re.compile(r"^[0-9a-f]{64}$")


class Invalid(Exception):
    pass


def reject(message):
    raise Invalid(message)


def read_json(path, what):
    try:
        with open(path, encoding="ascii") as handle:
            return json.load(handle)
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        reject(f"cannot read {what}: {error}")


def digest(path):
    value = hashlib.sha256()
    try:
        with open(path, "rb") as handle:
            for chunk in iter(lambda: handle.read(1024 * 1024), b""):
                value.update(chunk)
    except OSError as error:
        reject(f"cannot read bytes {path}: {error}")
    return value.hexdigest()


def valid_name(value, what):
    if (
        not isinstance(value, str)
        or not value
        or value in (".", "..")
        or "/" in value
        or "\\" in value
        or not all(0x20 <= ord(char) <= 0x7E for char in value)
    ):
        reject(f"{what} is not a safe release filename")
    return value


def expected_inventory(path, release_files):
    value = read_json(path, "expected asset inventory")
    if not isinstance(value, list) or not value:
        reject("expected asset inventory is not a nonempty array")
    result = []
    names = set()
    for index, entry in enumerate(value):
        if not isinstance(entry, dict) or list(entry) != ["name", "size", "sha256"]:
            reject(f"expected asset {index} has the wrong keys")
        name = valid_name(entry["name"], f"expected asset {index}.name")
        if name in names:
            reject(f"duplicate expected asset name {name}")
        names.add(name)
        size = entry["size"]
        if not isinstance(size, int) or isinstance(size, bool) or size <= 0:
            reject(f"expected asset {name} has invalid size")
        sha256 = entry["sha256"]
        if not isinstance(sha256, str) or not HEX64.fullmatch(sha256):
            reject(f"expected asset {name} has invalid sha256")
        result.append({"name": name, "size": size, "sha256": sha256})

    try:
        entries = list(os.scandir(release_files))
    except OSError as error:
        reject(f"cannot enumerate release files: {error}")
    observed = {entry.name for entry in entries}
    if observed != names:
        reject(f"release file names {sorted(observed)} differ from expected {sorted(names)}")
    for entry in entries:
        if not entry.is_file(follow_symlinks=False):
            reject(f"release file {entry.name} is not a regular file")
    for expected in result:
        file_path = os.path.join(release_files, expected["name"])
        if os.path.getsize(file_path) != expected["size"]:
            reject(f"local release file {expected['name']} size differs")
        if digest(file_path) != expected["sha256"]:
            reject(f"local release file {expected['name']} digest differs")
    return result


def starter_deletion_counts(path, expected_names):
    value = read_json(path, "starter deletion counts")
    if not isinstance(value, dict):
        reject("starter deletion counts are not an object")
    counts = {}
    for name, count in value.items():
        valid_name(name, "starter deletion count name")
        if name not in expected_names:
            reject(f"starter deletion count names unexpected asset {name}")
        if not isinstance(count, int) or isinstance(count, bool) or not 0 <= count <= 2:
            reject(f"starter deletion count for {name} is outside 0..2")
        counts[name] = count
    return counts


def release_inventory(path, expected_by_name, downloads):
    value = read_json(path, "release asset inventory")
    if not isinstance(value, list):
        reject("release asset inventory is not an array")
    by_name = {}
    ids = set()
    for index, entry in enumerate(value):
        if not isinstance(entry, dict) or list(entry) != ["id", "name", "state", "size"]:
            reject(f"release asset {index} has the wrong keys")
        asset_id = entry["id"]
        if not isinstance(asset_id, int) or isinstance(asset_id, bool) or asset_id <= 0:
            reject(f"release asset {index} has invalid id")
        if asset_id in ids:
            reject(f"duplicate release asset id {asset_id}")
        ids.add(asset_id)
        name = valid_name(entry["name"], f"release asset {index}.name")
        if name in by_name:
            reject(f"duplicate release asset name {name}")
        if name not in expected_by_name:
            reject(f"unexpected release asset {name}")
        size = entry["size"]
        if not isinstance(size, int) or isinstance(size, bool) or size < 0:
            reject(f"release asset {name} has invalid API size")
        state = entry["state"]
        if state == "uploaded":
            expectation = expected_by_name[name]
            if size != expectation["size"]:
                reject(f"existing release asset {name} API size conflicts")
            download = os.path.join(downloads, str(asset_id))
            if not os.path.isfile(download) or os.path.islink(download):
                reject(f"downloaded bytes are missing for release asset {name}")
            by_name[name] = {
                "id": asset_id,
                "state": state,
                "size": size,
                "download": download,
            }
        elif state == "starter":
            by_name[name] = {"id": asset_id, "state": state, "size": size}
        else:
            reject(f"release asset {name} has unknown state {state!r}")
    return by_name


def evaluate(args):
    expected = expected_inventory(args.expected, args.release_files)
    expected_by_name = {entry["name"]: entry for entry in expected}
    if args.release_draft not in ("true", "false"):
        reject("release draft state must be literal true or false")
    is_draft = args.release_draft == "true"
    deletion_counts = starter_deletion_counts(args.starter_deletions, set(expected_by_name))
    existing = release_inventory(args.assets, expected_by_name, args.downloads)
    for entry in expected:
        name = entry["name"]
        asset = existing.get(name)
        if asset is None or asset["state"] != "starter":
            continue
        if args.verb == "verify-complete":
            reject(f"release asset {name} is an incomplete starter")
        if not is_draft:
            reject(f"published release asset {name} is an incomplete starter")
        if deletion_counts.get(name, 0) >= 2:
            reject(f"starter deletion bound exhausted for release asset {name}")
        return {
            "complete": False,
            "action": {"kind": "delete-starter", "name": name, "id": asset["id"]},
        }
    for name, asset in existing.items():
        if asset["state"] != "uploaded":
            reject(f"release asset {name} did not resolve to uploaded bytes")
        expectation = expected_by_name[name]
        if os.path.getsize(asset["download"]) != expectation["size"]:
            reject(f"existing release asset {name} size conflicts")
        if digest(asset["download"]) != expectation["sha256"]:
            reject(f"existing release asset {name} digest conflicts")
    missing = [entry["name"] for entry in expected if entry["name"] not in existing]
    if args.verb == "verify-complete" and missing:
        reject(f"release is incomplete; missing {missing}")
    if missing:
        return {"complete": False, "action": {"kind": "upload", "name": missing[0]}}
    return {"complete": True}


def parser():
    root = argparse.ArgumentParser()
    root.add_argument("verb", choices=("plan", "verify-complete"))
    root.add_argument("--release-files", required=True)
    root.add_argument("--expected", required=True)
    root.add_argument("--assets", required=True)
    root.add_argument("--downloads", required=True)
    root.add_argument("--release-draft", required=True)
    root.add_argument("--starter-deletions", required=True)
    return root


def main():
    args = parser().parse_args()
    try:
        print(json.dumps(evaluate(args), sort_keys=True))
    except Invalid as error:
        print(f"release-asset-transaction: {error}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
