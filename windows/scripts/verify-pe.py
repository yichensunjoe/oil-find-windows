"""Verify architecture, GUI subsystem and embedded manifest in a Windows PE."""
import argparse
import pathlib
import struct


def main():
    p = argparse.ArgumentParser()
    p.add_argument("exe", type=pathlib.Path)
    p.add_argument("--arch", choices=("amd64", "arm64"), default="amd64")
    args = p.parse_args()
    data = args.exe.read_bytes()
    assert data[:2] == b"MZ", "Missing DOS signature"
    pe = struct.unpack_from("<I", data, 0x3C)[0]
    assert data[pe:pe+4] == b"PE\0\0", "Missing PE signature"
    machine, count = struct.unpack_from("<HH", data, pe+4)
    assert machine == (0x8664 if args.arch == "amd64" else 0xAA64), "Wrong architecture"
    optional_size = struct.unpack_from("<H", data, pe+20)[0]
    optional = pe + 24
    assert struct.unpack_from("<H", data, optional)[0] == 0x20B, "Not a 64-bit PE"
    assert struct.unpack_from("<H", data, optional+68)[0] == 2, "Not Windows GUI subsystem"
    resource_rva, resource_size = struct.unpack_from("<II", data, optional+112+2*8)
    assert resource_rva and resource_size, "Resource directory missing"
    sections = []
    for i in range(count):
        off = optional + optional_size + i*40
        name, vs, rva, size, fileoff = struct.unpack_from("<8sIIII", data, off)
        sections.append((name, rva, max(vs, size), fileoff))
    def raw(rva):
        for _, start, size, fileoff in sections:
            if start <= rva < start+size:
                return fileoff + rva - start
        raise AssertionError("RVA outside sections")
    base = raw(resource_rva)
    def child(offset, wanted):
        _, _, _, _, named, ids = struct.unpack_from("<IIHHHH", data, base+offset)
        for i in range(named+ids):
            ident, location = struct.unpack_from("<II", data, base+offset+16+i*8)
            if ident == wanted:
                return location
        raise AssertionError("Manifest resource entry missing")
    typ = child(0, 24)
    ident = child(typ & 0x7FFFFFFF, 1)
    language = child(ident & 0x7FFFFFFF, 0)
    content_rva, size, _, _ = struct.unpack_from("<IIII", data, base+language)
    manifest = data[raw(content_rva):raw(content_rva)+size]
    assert b"asInvoker" in manifest and b"requireAdministrator" not in manifest
    assert b"Microsoft.Windows.Common-Controls" in manifest
    assert b"PerMonitorV2" in manifest
    print("PE verified:", args.exe.name, args.arch, "GUI, embedded v6/asInvoker/DPI manifest")


if __name__ == "__main__":
    main()
