#!/usr/bin/env python3
"""Deterministic rejection matrix for candidate-manifest.py.

Builds one fully valid synthetic record set, asserts assembly succeeds and
is byte-stable, then applies one mutation at a time — every individually
missing required (gate, lane), a different-byte verification, a wrong
commit, an unknown lane, an unknown gate, a duplicated pair, a foreign-run
build reference, a malformed digest, a CR-bearing field, an oversized
size — and asserts each single mutation refuses the manifest.
"""

import hashlib
import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
ASSEMBLER = os.path.join(HERE, "candidate-manifest.py")

spec = importlib.util.spec_from_file_location("assembler", ASSEMBLER)
assembler = importlib.util.module_from_spec(spec)
spec.loader.exec_module(assembler)

COMMIT = "1" * 40
VERSION = "v0.1.0"
ENV = {**os.environ, "COMMIT": COMMIT, "VERSION": VERSION, "RUN_ID": "500", "RUN_ATTEMPT": "1"}

# One deterministic native lane per closure entry for job identity.
JOB_SEQ = {}


def write_valid(records: str) -> None:
    os.makedirs(records, exist_ok=True)
    for target in assembler.TARGETS:
        sha = hashlib.sha256(target.encode()).hexdigest()
        build = {
            "asset": f"moor-0.1.0-{assembler.ASSET_SUFFIX[target]}",
            "sha256": sha,
            "size": 4200000,
            "artifactId": str(1000 + assembler.TARGETS.index(target)),
            "artifactName": f"moor-candidate-{target}",
            "workflowRunId": "500",
            "workflowRunAttempt": 1,
            "jobId": str(10 + assembler.TARGETS.index(target)),
            "jobName": f"Build {target}",
        }
        with open(os.path.join(records, f"build-{target}.json"), "w") as handle:
            json.dump(build, handle)
        job = 100
        for gate, lanes in assembler.REQUIRED[target].items():
            for lane in sorted(lanes):
                job += 1
                record = {
                    "gate": gate,
                    "lane": lane,
                    "commit": COMMIT,
                    "sha256": sha,
                    "workflowRunId": "500",
                    "workflowRunAttempt": 1,
                    "jobId": str(job),
                    "jobName": f"Verify {target} / {gate} / {lane}",
                }
                with open(os.path.join(records, f"verify-{target}-{gate}-{lane}.json"), "w") as handle:
                    json.dump(record, handle)


def run(records: str, out: str):
    return subprocess.run(
        [sys.executable, ASSEMBLER, "--records", records, "--out", out],
        env=ENV,
        capture_output=True,
        text=True,
    )


def expect_reject(records: str, label: str) -> None:
    result = run(records, os.path.join(records, "_out"))
    assert result.returncode != 0, f"{label}: unexpectedly accepted"
    assert result.stderr.strip(), f"{label}: no diagnostic"


def mutate_json(path: str, **changes):
    with open(path) as handle:
        value = json.load(handle)
    value.update(changes)
    with open(path) as handle:
        pass
    with open(path, "w") as handle:
        json.dump(value, handle)


def main() -> None:
    base = tempfile.mkdtemp(prefix="manifest-test-")
    good = os.path.join(base, "good")
    write_valid(good)
    result = run(good, os.path.join(base, "out1"))
    assert result.returncode == 0, f"valid set rejected: {result.stderr}"
    result2 = run(good, os.path.join(base, "out2"))
    assert result2.returncode == 0
    with open(os.path.join(base, "out1", "moor-release-manifest-v1.json"), "rb") as handle:
        first = handle.read()
    with open(os.path.join(base, "out2", "moor-release-manifest-v1.json"), "rb") as handle:
        assert first == handle.read(), "assembly is not byte-stable"
    assert b"\r" not in first
    with open(os.path.join(base, "out1", "SHA256SUMS"), "rb") as handle:
        sums = handle.read()
    assert b"\r" not in sums and sums.count(b"\n") == 6 and sums.endswith(b"\n")

    # The narrowed closure must be honest, not silent: a run with no deferred
    # record is accepted, and the manifest names every lane it could not cover.
    manifest = json.loads(first)
    deferred_pairs = sorted(
        (target, gate, lane)
        for target, gates in assembler.DEFERRED.items()
        for gate, lanes in gates.items()
        for lane in lanes
    )
    # The emitted shape is pinned to docs/release-manifest-v1.md key-for-key, so
    # the producer cannot drift from the contract it claims to implement. A v1
    # object has an exact key set in an exact order, and a consumer rejects any
    # extension — so an added key has to change the document first.
    assert list(manifest) == [
        "schemaVersion",
        "repository",
        "version",
        "commit",
        "candidate",
        "coverage",
        "targets",
    ], list(manifest)
    assert manifest["schemaVersion"] == 1
    assert list(manifest["coverage"]) == ["requiredClosure", "unverified"], list(
        manifest["coverage"]
    )
    for entry in manifest["coverage"]["unverified"]:
        assert list(entry) == ["target", "gate", "lane"], list(entry)
    assert manifest["coverage"]["unverified"] == sorted(
        manifest["coverage"]["unverified"],
        key=lambda entry: (entry["target"], entry["gate"], entry["lane"]),
    ), "unverified is not in the documented ascending order"

    assert manifest["coverage"]["requiredClosure"] == "hosted-only", manifest["coverage"]
    assert [
        (entry["target"], entry["gate"], entry["lane"])
        for entry in manifest["coverage"]["unverified"]
    ] == deferred_pairs, "manifest does not name exactly the uncovered deferred lanes"

    # A deferred lane is optional, not unchecked: a real record for one is
    # accepted, and that lane then disappears from the unverified list.
    covered_one = os.path.join(base, "deferred-one")
    shutil.copytree(good, covered_one)
    target, gate, lane = deferred_pairs[0]
    sha = hashlib.sha256(target.encode()).hexdigest()
    with open(os.path.join(covered_one, f"verify-{target}-{gate}-{lane}.json"), "w") as handle:
        json.dump(
            {
                "gate": gate,
                "lane": lane,
                "commit": COMMIT,
                "sha256": sha,
                "workflowRunId": "500",
                "workflowRunAttempt": 1,
                "jobId": "901",
                "jobName": f"Verify {target} / {gate} / {lane}",
            },
            handle,
        )
    result = run(covered_one, os.path.join(base, "out-deferred-one"))
    assert result.returncode == 0, f"covered deferred lane rejected: {result.stderr}"
    with open(os.path.join(base, "out-deferred-one", "moor-release-manifest-v1.json")) as handle:
        partial = json.load(handle)
    assert [
        (entry["target"], entry["gate"], entry["lane"])
        for entry in partial["coverage"]["unverified"]
    ] == deferred_pairs[1:], "covering a deferred lane did not clear it"
    # The label follows which pairs are missing, not merely that some are: a
    # candidate holding evidence for part of the deferred set is no longer
    # hosted-only, and must not keep claiming to be.
    assert partial["coverage"]["requiredClosure"] == "partial", partial["coverage"]
    assert list(partial["coverage"]) == ["requiredClosure", "unverified"], list(
        partial["coverage"]
    )

    # Covering every deferred lane (the restored full matrix) must not emit an
    # empty array — the serializer refuses one, so the key is dropped instead.
    covered_all = os.path.join(base, "deferred-all")
    shutil.copytree(good, covered_all)
    for index, (target, gate, lane) in enumerate(deferred_pairs):
        sha = hashlib.sha256(target.encode()).hexdigest()
        with open(os.path.join(covered_all, f"verify-{target}-{gate}-{lane}.json"), "w") as handle:
            json.dump(
                {
                    "gate": gate,
                    "lane": lane,
                    "commit": COMMIT,
                    "sha256": sha,
                    "workflowRunId": "500",
                    "workflowRunAttempt": 1,
                    "jobId": str(910 + index),
                    "jobName": f"Verify {target} / {gate} / {lane}",
                },
                handle,
            )
    result = run(covered_all, os.path.join(base, "out-deferred-all"))
    assert result.returncode == 0, f"full matrix rejected: {result.stderr}"
    with open(os.path.join(base, "out-deferred-all", "moor-release-manifest-v1.json")) as handle:
        full = json.load(handle)
    assert full["coverage"] == {"requiredClosure": "full-matrix"}, full["coverage"]
    assert list(full["coverage"]) == ["requiredClosure"], list(full["coverage"])
    assert list(full) == list(manifest), "the full-matrix top level drifted from the narrowed one"

    # Anti-drift: every label the producer can emit must be documented in both
    # normative files. A new or renamed label that only lands in code fails
    # here, so the documents cannot silently fall behind the producer.
    emitted_labels = {
        manifest["coverage"]["requiredClosure"],
        partial["coverage"]["requiredClosure"],
        full["coverage"]["requiredClosure"],
    }
    assert emitted_labels == {"hosted-only", "partial", "full-matrix"}, emitted_labels
    for relative in ("release-manifest-v1.md", "release-matrix.md"):
        path = os.path.join(HERE, os.pardir, "docs", relative)
        with open(path, encoding="utf-8") as handle:
            text = handle.read()
        for label in sorted(emitted_labels):
            assert f'"{label}"' in text, f"{relative} does not document the {label} closure"

    cases = 0

    # A deferred lane still has to be a real verification of these exact bytes.
    for label, mutation in (
        ("deferred lane with a different digest", {"sha256": "f" * 64}),
        ("deferred lane with a wrong commit", {"commit": "2" * 40}),
    ):
        trial = os.path.join(base, f"deferred-bad-{cases}")
        shutil.copytree(good, trial)
        target, gate, lane = deferred_pairs[0]
        record = {
            "gate": gate,
            "lane": lane,
            "commit": COMMIT,
            "sha256": hashlib.sha256(target.encode()).hexdigest(),
            "workflowRunId": "500",
            "workflowRunAttempt": 1,
            "jobId": "902",
            "jobName": f"Verify {target} / {gate} / {lane}",
        }
        record.update(mutation)
        with open(os.path.join(trial, f"verify-{target}-{gate}-{lane}.json"), "w") as handle:
            json.dump(record, handle)
        expect_reject(trial, label)
        cases += 1

    # Every individually missing required (gate, lane) must refuse.
    for target in assembler.TARGETS:
        for gate, lanes in assembler.REQUIRED[target].items():
            for lane in sorted(lanes):
                trial = os.path.join(base, f"missing-{cases}")
                shutil.copytree(good, trial)
                os.remove(os.path.join(trial, f"verify-{target}-{gate}-{lane}.json"))
                expect_reject(trial, f"missing {target}/{gate}/{lane}")
                cases += 1

    target = assembler.TARGETS[0]
    lane = "ubuntu-22.04-x64"
    sample = f"verify-{target}-identity-{lane}.json"

    def trial_with(label: str, mutator):
        nonlocal cases
        trial = os.path.join(base, f"case-{cases}")
        shutil.copytree(good, trial)
        mutator(trial)
        expect_reject(trial, label)
        cases += 1

    trial_with("different-byte verification", lambda t: mutate_json(os.path.join(t, sample), sha256="f" * 64))
    trial_with("wrong verified commit", lambda t: mutate_json(os.path.join(t, sample), commit="2" * 40))
    trial_with("unknown lane", lambda t: mutate_json(os.path.join(t, sample), lane="ubuntu-20.04-x64"))
    trial_with("unknown gate", lambda t: mutate_json(os.path.join(t, sample), gate="smoke"))
    # A real lane, but for a target/gate the matrix does not assign it to:
    # a Linux identity lane presented on the Windows x64 target.
    trial_with(
        "foreign-target lane on gate",
        lambda t: mutate_json(
            os.path.join(t, "verify-x86_64-pc-windows-msvc-identity-windows-2022-x64.json"),
            lane="ubuntu-22.04-x64",
        ),
    )
    trial_with(
        "duplicate (gate, lane)",
        lambda t: shutil.copyfile(os.path.join(t, sample), os.path.join(t, f"verify-{target}-identity-{lane}.copy.json")),
    )
    trial_with(
        "foreign-run build reference",
        lambda t: mutate_json(os.path.join(t, f"build-{target}.json"), workflowRunId="9999"),
    )
    trial_with(
        "malformed build digest",
        lambda t: mutate_json(os.path.join(t, f"build-{target}.json"), sha256="ABCD"),
    )
    trial_with(
        "carriage return in job name",
        lambda t: mutate_json(os.path.join(t, sample), jobName="bad\rname"),
    )
    trial_with(
        "oversized size",
        lambda t: mutate_json(os.path.join(t, f"build-{target}.json"), size=9007199254740992),
    )
    trial_with(
        "wrong artifact name",
        lambda t: mutate_json(os.path.join(t, f"build-{target}.json"), artifactName="moor-candidate-oops"),
    )
    trial_with(
        "missing build record",
        lambda t: os.remove(os.path.join(t, f"build-{target}.json")),
    )

    print(f"candidate-manifest-test: OK ({cases} rejection cases + byte-stable accept)")


if __name__ == "__main__":
    main()
