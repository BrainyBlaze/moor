#!/usr/bin/env python3
"""Deterministic rejection matrix for candidate-manifest.py.

Builds one fully valid synthetic record set, asserts assembly succeeds and
is byte-stable, then applies one mutation at a time — every individually
missing required (gate, lane), a different-byte verification, a wrong
commit, an unknown lane, an unknown gate, a duplicated pair, a foreign-run
build reference, a malformed digest, a CR-bearing field, an oversized
size — and asserts each single mutation refuses the manifest.
"""

import contextlib
import hashlib
import importlib.util
import io
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


# A deferred set that exists only in this test, so the coverage mechanism
# (hosted-only / partial / unverified) is exercised while production defers
# nothing. Two lanes on one gate are the minimum that distinguishes "partial"
# from "hosted-only".
SYNTHETIC_DEFERRED = {
    "aarch64-unknown-linux-musl": {"compatibility": {"enrolled-lane-a", "enrolled-lane-b"}},
}


@contextlib.contextmanager
def deferred_set(deferred: dict):
    """Swap the assembler's deferred table (and the permitted set derived from
    it) for the duration of the block; the module is restored afterwards so the
    subprocess-driven cases keep seeing production data."""
    saved = (assembler.DEFERRED, assembler.PERMITTED)
    assembler.DEFERRED = deferred
    assembler.PERMITTED = {
        (target, gate, lane)
        for table in (assembler.REQUIRED, deferred)
        for target, gates in table.items()
        for gate, lanes in gates.items()
        for lane in lanes
    }
    try:
        yield
    finally:
        assembler.DEFERRED, assembler.PERMITTED = saved


def assemble_in_process(records: str, out: str):
    """Run assembler.main() in this interpreter with the same argv/env the
    subprocess cases use. Returns (exit code, parsed manifest or None, stderr)."""
    argv, environ = sys.argv, dict(os.environ)
    sys.argv = [ASSEMBLER, "--records", records, "--out", out]
    os.environ.update({key: ENV[key] for key in ("COMMIT", "VERSION", "RUN_ID", "RUN_ATTEMPT")})
    stdout, stderr = io.StringIO(), io.StringIO()
    code = 0
    try:
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            try:
                assembler.main()
            except SystemExit as exit_:
                code = exit_.code if isinstance(exit_.code, int) else 1
    finally:
        sys.argv = argv
        os.environ.clear()
        os.environ.update(environ)
    manifest = None
    if code == 0:
        with open(os.path.join(out, "moor-release-manifest-v1.json"), encoding="ascii") as handle:
            manifest = json.load(handle)
    return code, manifest, stderr.getvalue()


def write_verify(records: str, target: str, gate: str, lane: str, job_id: str, **overrides) -> None:
    record = {
        "gate": gate,
        "lane": lane,
        "commit": COMMIT,
        "sha256": hashlib.sha256(target.encode()).hexdigest(),
        "workflowRunId": "500",
        "workflowRunAttempt": 1,
        "jobId": job_id,
        "jobName": f"Verify {target} / {gate} / {lane}",
    }
    record.update(overrides)
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
    assert b"\r" not in sums and sums.count(b"\n") == len(assembler.TARGETS) and sums.endswith(b"\n")

    # The deferred set is empty, so the required closure is the whole matrix:
    # a candidate that carries it states "full-matrix" and carries no
    # unverified list. An honest label is still asserted here rather than
    # assumed — the producer, not the test, decides what it emitted.
    manifest = json.loads(first)
    assert assembler.DEFERRED == {}, "the deferred set is documented as empty"
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
    assert manifest["coverage"] == {"requiredClosure": "full-matrix"}, manifest["coverage"]
    assert list(manifest["coverage"]) == ["requiredClosure"], list(manifest["coverage"])

    # `coverage` is part of the consumer pin contract, so the mechanism behind
    # it is exercised even though production defers nothing: the assembler runs
    # in-process against a synthetic two-lane deferred set. Only the data table
    # is swapped — the producer has no test-only code path.
    deferred_pairs = sorted(
        (target, gate, lane)
        for target, gates in SYNTHETIC_DEFERRED.items()
        for gate, lanes in gates.items()
        for lane in lanes
    )
    with deferred_set(SYNTHETIC_DEFERRED):
        # The narrowed closure must be honest, not silent: a run with no
        # deferred record is accepted, and the manifest names every lane it
        # could not cover.
        code, narrowed, stderr = assemble_in_process(good, os.path.join(base, "out-hosted-only"))
        assert code == 0, f"hosted-only set rejected: {stderr}"
        assert list(narrowed) == list(manifest), "the narrowed top level drifted from the full-matrix one"
        assert list(narrowed["coverage"]) == ["requiredClosure", "unverified"], list(
            narrowed["coverage"]
        )
        for entry in narrowed["coverage"]["unverified"]:
            assert list(entry) == ["target", "gate", "lane"], list(entry)
        assert narrowed["coverage"]["unverified"] == sorted(
            narrowed["coverage"]["unverified"],
            key=lambda entry: (entry["target"], entry["gate"], entry["lane"]),
        ), "unverified is not in the documented ascending order"
        assert narrowed["coverage"]["requiredClosure"] == "hosted-only", narrowed["coverage"]
        assert [
            (entry["target"], entry["gate"], entry["lane"])
            for entry in narrowed["coverage"]["unverified"]
        ] == deferred_pairs, "manifest does not name exactly the uncovered deferred lanes"

        # A deferred lane is optional, not unchecked: a real record for one is
        # accepted, and that lane then disappears from the unverified list.
        covered_one = os.path.join(base, "deferred-one")
        shutil.copytree(good, covered_one)
        target, gate, lane = deferred_pairs[0]
        write_verify(covered_one, target, gate, lane, "901")
        code, partial, stderr = assemble_in_process(covered_one, os.path.join(base, "out-deferred-one"))
        assert code == 0, f"covered deferred lane rejected: {stderr}"
        assert [
            (entry["target"], entry["gate"], entry["lane"])
            for entry in partial["coverage"]["unverified"]
        ] == deferred_pairs[1:], "covering a deferred lane did not clear it"
        # The label follows which pairs are missing, not merely that some are:
        # a candidate holding evidence for part of the deferred set is no
        # longer hosted-only, and must not keep claiming to be.
        assert partial["coverage"]["requiredClosure"] == "partial", partial["coverage"]
        assert list(partial["coverage"]) == ["requiredClosure", "unverified"], list(
            partial["coverage"]
        )

        # Covering every deferred lane (the restored full matrix) must not emit
        # an empty array — the serializer refuses one, so the key is dropped.
        covered_all = os.path.join(base, "deferred-all")
        shutil.copytree(good, covered_all)
        for index, (target, gate, lane) in enumerate(deferred_pairs):
            write_verify(covered_all, target, gate, lane, str(910 + index))
        code, full, stderr = assemble_in_process(covered_all, os.path.join(base, "out-deferred-all"))
        assert code == 0, f"full matrix rejected: {stderr}"
        assert full["coverage"] == {"requiredClosure": "full-matrix"}, full["coverage"]
        assert list(full["coverage"]) == ["requiredClosure"], list(full["coverage"])
        assert list(full) == list(manifest), "the covered top level drifted from the full-matrix one"

        # Anti-drift: every label the producer can emit must be documented in
        # both normative files. A new or renamed label that only lands in code
        # fails here, so the documents cannot silently fall behind the producer.
        emitted_labels = {
            manifest["coverage"]["requiredClosure"],
            narrowed["coverage"]["requiredClosure"],
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

        # A deferred lane still has to be a real verification of these exact
        # bytes: the refusal must come from the digest/commit check, not from
        # the lane being unknown, so it is asserted against the same table.
        for index, (label, mutation) in enumerate(
            (
                ("deferred lane with a different digest", {"sha256": "f" * 64}),
                ("deferred lane with a wrong commit", {"commit": "2" * 40}),
            )
        ):
            trial = os.path.join(base, f"deferred-bad-{index}")
            shutil.copytree(good, trial)
            target, gate, lane = deferred_pairs[0]
            write_verify(trial, target, gate, lane, "902", **mutation)
            code, _, stderr = assemble_in_process(trial, os.path.join(trial, "_out"))
            assert code != 0, f"{label}: unexpectedly accepted"
            assert stderr.strip(), f"{label}: no diagnostic"

    # Outside the injection the synthetic lane is not a matrix lane at all: a
    # record for it is refused, so the production table cannot be widened by
    # a stray verification file.
    stray = os.path.join(base, "stray-deferred")
    shutil.copytree(good, stray)
    target, gate, lane = deferred_pairs[0]
    write_verify(stray, target, gate, lane, "903")
    expect_reject(stray, "verification record for a lane outside the matrix")

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
    # a Linux identity lane presented on the macOS arm64 target.
    trial_with(
        "foreign-target lane on gate",
        lambda t: mutate_json(
            os.path.join(t, "verify-aarch64-apple-darwin-identity-macos-15-arm64.json"),
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
