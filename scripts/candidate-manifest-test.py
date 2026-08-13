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

    cases = 0

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
