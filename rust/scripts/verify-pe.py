"""Verify architecture, GUI subsystem and embedded manifest in a Windows PE."""

import argparse
import pathlib
import struct

# 资源目录里的清单类型/ID：RT_MANIFEST = 24，ID 1 = CREATEPROCESS_MANIFEST_RESOURCE_ID。
RT_MANIFEST = 24
MANIFEST_ID = 1


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("exe", type=pathlib.Path)
    parser.add_argument("--arch", choices=("amd64", "arm64"), default="amd64")
    args = parser.parse_args()

    data = args.exe.read_bytes()
    assert data[:2] == b"MZ", "Missing DOS signature"
    pe = struct.unpack_from("<I", data, 0x3C)[0]
    assert data[pe : pe + 4] == b"PE\0\0", "Missing PE signature"
    machine, count = struct.unpack_from("<HH", data, pe + 4)
    assert machine == (0x8664 if args.arch == "amd64" else 0xAA64), "Wrong architecture"
    optional_size = struct.unpack_from("<H", data, pe + 20)[0]
    optional = pe + 24
    assert struct.unpack_from("<H", data, optional)[0] == 0x20B, "Not a 64-bit PE"
    # 2 = IMAGE_SUBSYSTEM_WINDOWS_GUI：双击时不弹控制台窗口。
    assert struct.unpack_from("<H", data, optional + 68)[0] == 2, "Not Windows GUI subsystem"
    resource_rva, resource_size = struct.unpack_from("<II", data, optional + 112 + 2 * 8)
    assert resource_rva and resource_size, "Resource directory missing"

    sections = []
    for index in range(count):
        offset = optional + optional_size + index * 40
        name, _, rva, size, file_offset = struct.unpack_from("<8sIIII", data, offset)
        # 用 SizeOfRawData 映射：VirtualSize 可能大于文件里实际存在的字节，
        # 会把下一个节区的范围也算进来。
        sections.append((name, rva, size, file_offset))

    def raw(rva):
        for _, start, size, file_offset in sections:
            if start <= rva < start + size and file_offset + (rva - start) < len(data):
                return file_offset + rva - start
        raise AssertionError("RVA outside file-backed sections")

    base = raw(resource_rva)

    def entries(offset):
        _, _, _, _, named, ids = struct.unpack_from("<IIHHHH", data, base + offset)
        return [
            struct.unpack_from("<II", data, base + offset + 16 + index * 8)
            for index in range(named + ids)
        ]

    def child(offset, wanted):
        for identifier, location in entries(offset):
            if identifier == wanted:
                return location
        raise AssertionError("Manifest resource entry missing")

    manifest_type = child(0, RT_MANIFEST)
    manifest_name = child(manifest_type & 0x7FFFFFFF, MANIFEST_ID)
    # 语言目录：资源编译器可能用 0、1033 或 2052，取实际存在的那一个。
    language_entries = entries(manifest_name & 0x7FFFFFFF)
    assert language_entries, "Manifest language entry missing"
    content_rva, size, _, _ = struct.unpack_from(
        "<IIII", data, base + language_entries[0][1]
    )
    manifest = data[raw(content_rva) : raw(content_rva) + size]

    assert b"asInvoker" in manifest, "Manifest does not request asInvoker"
    assert b"requireAdministrator" not in manifest, "Manifest requests elevation"
    assert b"Microsoft.Windows.Common-Controls" in manifest, "Common-Controls v6 missing"
    assert b"PerMonitorV2" in manifest, "PerMonitorV2 DPI awareness missing"
    print(
        "PE verified:",
        args.exe.name,
        args.arch,
        "GUI, embedded v6/asInvoker/DPI manifest",
    )


if __name__ == "__main__":
    main()
