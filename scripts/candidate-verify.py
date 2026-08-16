#!/usr/bin/env python3
"""Exact-byte audits for release-candidate assets, with built-in negatives.

Subcommands (each exits nonzero on any violation):

  zip-inventory <zip> <name> [<name2>]   the archive holds exactly the named
                                         regular-file members: no directory
                                         entries, no links, no hidden or
                                         extra members, no path separators.
  macho-minos <file> <major.minor>       the Mach-O minimum OS equals the
                                         frozen deployment target exactly.
  self-test                              run every parser against crafted
                                         positive and adversarial inputs;
                                         proves the checks can fail.

Everything is stdlib-only and byte-oriented so the same audit runs
identically on every runner against the exact downloaded asset bytes.
"""

import io
import struct
import sys
import zipfile



def fail(message: str) -> None:
    print(f"candidate-verify: {message}", file=sys.stderr)
    sys.exit(1)


# ---------------------------------------------------------------- zip


def zip_inventory(path: str, names: list) -> None:
    with zipfile.ZipFile(path) as archive:
        infos = archive.infolist()
        if len(infos) != len(names):
            fail(f"{path}: {len(infos)} members, expected {len(names)}")
        seen = []
        for info in infos:
            name = info.filename
            if name.endswith("/") or info.is_dir():
                fail(f"{path}: directory member {name!r}")
            if "/" in name or "\\" in name or name.startswith("."):
                fail(f"{path}: unsafe member name {name!r}")
            # Upper 16 bits of external_attr carry the POSIX mode. Require a
            # regular file: a set type field that is anything else — symlink,
            # FIFO, socket, block/char device, directory — is rejected. A zero
            # mode (archives that record no POSIX type) is allowed and treated
            # as a regular file; the directory guard above already caught dirs.
            mode = info.external_attr >> 16
            file_type = mode & 0o170000
            if file_type not in (0, 0o100000):
                fail(f"{path}: non-regular member {name!r} (type {file_type:o})")
            seen.append(name)
        if sorted(seen) != sorted(names):
            fail(f"{path}: members {sorted(seen)} != expected {sorted(names)}")


# ---------------------------------------------------------------- mach-o

LC_VERSION_MIN_MACOSX = 0x24
LC_BUILD_VERSION = 0x32


def macho_min_version(data: bytes) -> str:
    if data[:4] != b"\xcf\xfa\xed\xfe":
        fail("not a thin little-endian 64-bit Mach-O")
    (ncmds,) = struct.unpack_from("<I", data, 16)
    offset = 32
    for _ in range(ncmds):
        cmd, cmdsize = struct.unpack_from("<II", data, offset)
        if cmd == LC_BUILD_VERSION:
            (minos,) = struct.unpack_from("<I", data, offset + 12)
            return f"{minos >> 16}.{(minos >> 8) & 0xFF}.{minos & 0xFF}"
        if cmd == LC_VERSION_MIN_MACOSX:
            (version,) = struct.unpack_from("<I", data, offset + 8)
            return f"{version >> 16}.{(version >> 8) & 0xFF}.{version & 0xFF}"
        offset += cmdsize
    fail("no LC_BUILD_VERSION or LC_VERSION_MIN_MACOSX load command")


def macho_minos(path: str, expected: str) -> None:
    # Compare the full X.Y.Z minimum: a crafted 13.0.1 must not pass a 13.0.0
    # gate, so the patch byte is significant. A two-component expected is
    # normalised to X.Y.0.
    parts = expected.split(".")
    if len(parts) == 2:
        expected = f"{expected}.0"
    elif len(parts) != 3:
        fail(f"expected minimum must be X.Y or X.Y.Z, got {expected!r}")
    with open(path, "rb") as handle:
        data = handle.read()
    found = macho_min_version(data)
    print(f"{path}: minimum macOS {found}")
    if found != expected:
        fail(f"{path}: minimum macOS {found} != frozen {expected}")


# ---------------------------------------------------------------- self-test


def craft_macho(minos_hex: int) -> bytes:
    header = struct.pack("<IIIIIIII", 0xFEEDFACF, 0x0100000C, 0, 2, 1, 24, 0, 0)
    build = struct.pack("<IIIIII", LC_BUILD_VERSION, 24, 1, minos_hex, 0, 0)
    return header + build


def self_test() -> None:
    assert macho_min_version(craft_macho(0x000D0000)) == "13.0.0"
    assert macho_min_version(craft_macho(0x000C0000)) == "12.0.0"
    # The patch byte is significant: 13.0.1 must not satisfy a 13.0(.0) gate.
    assert macho_min_version(craft_macho(0x000D0001)) == "13.0.1"

    import subprocess
    import tempfile

    with tempfile.TemporaryDirectory(prefix="candidate-verify-") as scratch:
        ok_zip = f"{scratch}/ok.zip"
        with zipfile.ZipFile(ok_zip, "w") as archive:
            archive.writestr("moor-0.1.0-linux-x64", b"bytes")
        zip_inventory(ok_zip, ["moor-0.1.0-linux-x64"])

        bad_zip = f"{scratch}/bad.zip"
        with zipfile.ZipFile(bad_zip, "w") as archive:
            archive.writestr("moor-0.1.0-linux-x64", b"bytes")
            archive.writestr(".hidden", b"extra")

        # A ZIP whose sole expected member is a FIFO (not a regular file).
        fifo_zip = f"{scratch}/fifo.zip"
        with zipfile.ZipFile(fifo_zip, "w") as archive:
            info = zipfile.ZipInfo("moor-0.1.0-linux-x64")
            info.external_attr = (0o010000 | 0o644) << 16  # S_IFIFO
            archive.writestr(info, b"")
        macho_ok = f"{scratch}/ok.macho"
        macho_patch = f"{scratch}/patch.macho"
        with open(macho_ok, "wb") as handle:
            handle.write(craft_macho(0x000D0000))
        with open(macho_patch, "wb") as handle:
            handle.write(craft_macho(0x000D0001))
        # Positive control: the exact 13.0.0 binary passes the 13.0 gate.
        macho_minos(macho_ok, "13.0")

        must_reject = [
            ["zip-inventory", bad_zip, "moor-0.1.0-linux-x64"],
            ["zip-inventory", ok_zip, "other-name"],
            ["zip-inventory", fifo_zip, "moor-0.1.0-linux-x64"],
            ["macho-minos", macho_patch, "13.0"],
        ]
        for args in must_reject:
            result = subprocess.run([sys.executable, __file__, *args], capture_output=True)
            assert result.returncode != 0, args
    print("candidate-verify self-test: OK")


def main() -> None:
    if len(sys.argv) < 2:
        fail("missing subcommand")
    command, args = sys.argv[1], sys.argv[2:]
    if command == "zip-inventory" and len(args) in (2, 3):
        zip_inventory(args[0], args[1:])
    elif command == "macho-minos" and len(args) == 2:
        macho_minos(args[0], args[1])
    elif command == "self-test" and not args:
        self_test()
    else:
        fail(f"unknown usage: {sys.argv[1:]}")


if __name__ == "__main__":
    main()
