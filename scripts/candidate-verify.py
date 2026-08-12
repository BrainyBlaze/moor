#!/usr/bin/env python3
"""Exact-byte audits for release-candidate assets, with built-in negatives.

Subcommands (each exits nonzero on any violation):

  zip-inventory <zip> <name> [<name2>]   the archive holds exactly the named
                                         regular-file members: no directory
                                         entries, no links, no hidden or
                                         extra members, no path separators.
  pe-imports <file>                      the PE import table names no
                                         VCRUNTIME*/MSVCP*/redistributable
                                         CRT DLL (static-CRT proof).
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

BANNED_IMPORT_PREFIXES = ("vcruntime", "msvcp", "msvcr", "api-ms-win-crt")


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


# ---------------------------------------------------------------- pe


def pe_imported_dlls(data: bytes) -> list:
    if data[:2] != b"MZ":
        fail("not a PE file: missing MZ")
    (e_lfanew,) = struct.unpack_from("<I", data, 0x3C)
    if data[e_lfanew : e_lfanew + 4] != b"PE\0\0":
        fail("not a PE file: missing PE signature")
    coff = e_lfanew + 4
    (machine, nsections, _, _, _, opt_size, _) = struct.unpack_from("<HHIIIHH", data, coff)
    opt = coff + 20
    (magic,) = struct.unpack_from("<H", data, opt)
    if magic == 0x20B:  # PE32+
        ddir_offset, ddir_count_offset = opt + 112, opt + 108
    elif magic == 0x10B:  # PE32
        ddir_offset, ddir_count_offset = opt + 96, opt + 92
    else:
        fail(f"unknown optional-header magic {magic:#x}")
    (ddir_count,) = struct.unpack_from("<I", data, ddir_count_offset)
    if ddir_count < 2:
        return []
    import_rva, import_size = struct.unpack_from("<II", data, ddir_offset + 8)
    if import_rva == 0:
        return []

    sections = []
    section_at = opt + opt_size
    for index in range(nsections):
        base = section_at + index * 40
        vsize, vaddr, rsize, roffset = struct.unpack_from("<IIII", data, base + 8)
        sections.append((vaddr, max(vsize, rsize), roffset))

    def rva_to_offset(rva: int) -> int:
        for vaddr, size, roffset in sections:
            if vaddr <= rva < vaddr + size:
                return roffset + (rva - vaddr)
        fail(f"import RVA {rva:#x} maps to no section")

    dlls = []
    descriptor = rva_to_offset(import_rva)
    while True:
        entry = struct.unpack_from("<IIIII", data, descriptor)
        if all(field == 0 for field in entry):
            break
        name_offset = rva_to_offset(entry[3])
        end = data.index(b"\0", name_offset)
        dlls.append(data[name_offset:end].decode("ascii", "replace"))
        descriptor += 20
    return dlls


def pe_imports(path: str) -> None:
    with open(path, "rb") as handle:
        data = handle.read()
    dlls = pe_imported_dlls(data)
    banned = [d for d in dlls if d.lower().startswith(BANNED_IMPORT_PREFIXES)]
    print(f"{path}: imports {dlls}")
    if banned:
        fail(f"{path}: dynamic CRT dependency {banned}")


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


def craft_pe(dll_names: list) -> bytes:
    # One .idata section at RVA 0x1000 holding descriptors then names.
    descriptors = b"".join(
        struct.pack("<IIIII", 1, 0, 0, 0x1000 + 20 * (len(dll_names) + 1) + 64 * i, 1)
        for i in range(len(dll_names))
    ) + b"\0" * 20
    names = b"".join(name.encode().ljust(64, b"\0") for name in dll_names)
    section_data = descriptors + names
    opt = bytearray(240)
    struct.pack_into("<H", opt, 0, 0x20B)
    struct.pack_into("<I", opt, 108, 16)
    struct.pack_into("<II", opt, 112 + 8, 0x1000, len(section_data))
    section = bytearray(40)
    section[:6] = b".idata"
    struct.pack_into("<IIII", section, 8, len(section_data), 0x1000, len(section_data), 0x400)
    head = bytearray(0x400)
    head[:2] = b"MZ"
    struct.pack_into("<I", head, 0x3C, 0x80)
    head[0x80:0x84] = b"PE\0\0"
    struct.pack_into("<HHIIIHH", head, 0x84, 0x8664, 1, 0, 0, 0, len(opt), 0)
    base = 0x84 + 20
    head[base : base + len(opt)] = opt
    head[base + len(opt) : base + len(opt) + 40] = section
    return bytes(head) + section_data


def craft_macho(minos_hex: int) -> bytes:
    header = struct.pack("<IIIIIIII", 0xFEEDFACF, 0x0100000C, 0, 2, 1, 24, 0, 0)
    build = struct.pack("<IIIIII", LC_BUILD_VERSION, 24, 1, minos_hex, 0, 0)
    return header + build


def self_test() -> None:
    clean = craft_pe(["KERNEL32.dll", "ntdll.dll"])
    assert pe_imported_dlls(clean) == ["KERNEL32.dll", "ntdll.dll"]
    dirty = craft_pe(["KERNEL32.dll", "VCRUNTIME140.dll"])
    banned = [d for d in pe_imported_dlls(dirty) if d.lower().startswith(BANNED_IMPORT_PREFIXES)]
    assert banned == ["VCRUNTIME140.dll"], banned

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
    elif command == "pe-imports" and len(args) == 1:
        pe_imports(args[0])
    elif command == "macho-minos" and len(args) == 2:
        macho_minos(args[0], args[1])
    elif command == "self-test" and not args:
        self_test()
    else:
        fail(f"unknown usage: {sys.argv[1:]}")


if __name__ == "__main__":
    main()
