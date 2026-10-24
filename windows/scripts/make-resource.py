"""Emit a minimal architecture-specific COFF object embedding RT_MANIFEST.

No compiler, executable helper, external package or network access is required.
The object is generated, ignored by Git, and consumed by the Go Windows linker.
"""
import argparse
import pathlib
import struct


def build(manifest: bytes, machine: int) -> bytes:
    # A resource tree: type RT_MANIFEST=24 -> id CREATEPROCESS_MANIFEST_RESOURCE_ID=1
    # -> language neutral=0 -> IMAGE_RESOURCE_DATA_ENTRY.
    header = lambda: struct.pack("<IIHHHH", 0, 0, 0, 0, 0, 1)
    payload_offset = 88
    resource = bytearray()
    resource += header() + struct.pack("<II", 24, 0x80000000 | 24)
    resource += header() + struct.pack("<II", 1, 0x80000000 | 48)
    resource += header() + struct.pack("<II", 0, 72)
    resource += struct.pack("<IIII", payload_offset, len(manifest), 65001, 0)
    resource += manifest
    resource += bytes((-len(resource)) % 4)
    raw_offset = 20 + 40
    reloc_offset = raw_offset + len(resource)
    symbol_offset = reloc_offset + 10
    file_header = struct.pack("<HHIIIHH", machine, 1, 0, symbol_offset, 1, 0, 0)
    section = struct.pack("<8sIIIIIIHHI", b".rsrc\0\0\0", 0, 0, len(resource), raw_offset,
                          reloc_offset, 0, 1, 0, 0x40000040)
    # Resource RVA points at manifest bytes in the same section. ADDR32NB is
    # REL_AMD64=3 / REL_ARM64=2; zero symbol value retains the stored addend.
    relocation = struct.pack("<IIH", 72, 0, 3 if machine == 0x8664 else 2)
    symbol = struct.pack("<8sIhHBB", b".rsrc\0\0\0", 0, 1, 0, 3, 0)
    return file_header + section + resource + relocation + symbol + struct.pack("<I", 4)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--arch", choices=("amd64", "arm64"), default="amd64")
    parser.add_argument("--manifest", type=pathlib.Path, required=True)
    parser.add_argument("--output", type=pathlib.Path, required=True)
    args = parser.parse_args()
    data = args.manifest.read_bytes()
    if b"asInvoker" not in data or b"requireAdministrator" in data:
        raise SystemExit("Only an asInvoker manifest is permitted")
    args.output.write_bytes(build(data, 0x8664 if args.arch == "amd64" else 0xAA64))
    print("Embedded manifest COFF:", args.output)


if __name__ == "__main__":
    main()
