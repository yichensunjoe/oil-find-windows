"""Verify architecture, GUI subsystem and embedded manifest in a Windows PE."""

import argparse
import pathlib
import struct
import xml.etree.ElementTree as ET

# 资源目录里的清单类型/ID：RT_MANIFEST = 24，ID 1 = CREATEPROCESS_MANIFEST_RESOURCE_ID。
RT_MANIFEST = 24
MANIFEST_ID = 1


def require(condition, message):
    if not condition:
        raise ValueError(message)


def verify_manifest(manifest):
    # Ignore resource padding without stripping the last UTF-16 code unit.
    if manifest.startswith((b"\xff\xfe", b"\xfe\xff", b"<\0", b"\0<")):
        trailing = len(manifest) - len(manifest.rstrip(b"\0"))
        manifest = manifest[: len(manifest) - trailing // 2 * 2]
    else:
        manifest = manifest.rstrip(b"\0")
    try:
        root = ET.fromstring(manifest)
    except ET.ParseError as err:
        raise ValueError("Manifest is not valid XML") from err

    assembly = "{urn:schemas-microsoft-com:asm.v1}"
    trust = "{urn:schemas-microsoft-com:asm.v3}"
    settings = "{http://schemas.microsoft.com/SMI/2016/WindowsSettings}"
    require(root.tag == assembly + "assembly", "Invalid manifest assembly")

    levels = root.findall(
        f"./{trust}trustInfo/{trust}security/{trust}requestedPrivileges/"
        f"{trust}requestedExecutionLevel"
    )
    require(
        all(level.get("level") != "requireAdministrator" for level in levels),
        "Manifest requests elevation",
    )
    require(
        len(levels) == 1 and levels[0].get("level") == "asInvoker",
        "Manifest does not request asInvoker",
    )
    identities = root.findall(
        f"./{assembly}dependency/{assembly}dependentAssembly/{assembly}assemblyIdentity"
    )
    require(
        any(
            identity.get("name") == "Microsoft.Windows.Common-Controls"
            and identity.get("version") == "6.0.0.0"
            for identity in identities
        ),
        "Common-Controls v6 missing",
    )
    awareness = root.findall(
        f"./{trust}application/{trust}windowsSettings/{settings}dpiAwareness"
    )
    require(
        any((item.text or "").split(",")[0].strip() == "PerMonitorV2" for item in awareness),
        "PerMonitorV2 DPI awareness missing",
    )


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("exe", type=pathlib.Path)
    parser.add_argument("--arch", choices=("amd64", "arm64"), default="amd64")
    args = parser.parse_args()

    data = args.exe.read_bytes()
    require(data[:2] == b"MZ", "Missing DOS signature")
    pe = struct.unpack_from("<I", data, 0x3C)[0]
    require(data[pe : pe + 4] == b"PE\0\0", "Missing PE signature")
    machine, count = struct.unpack_from("<HH", data, pe + 4)
    require(machine == (0x8664 if args.arch == "amd64" else 0xAA64), "Wrong architecture")
    optional_size = struct.unpack_from("<H", data, pe + 20)[0]
    optional = pe + 24
    require(optional_size >= 112 + 3 * 8, "Optional header is too short")
    require(struct.unpack_from("<H", data, optional)[0] == 0x20B, "Not a 64-bit PE")
    # 2 = IMAGE_SUBSYSTEM_WINDOWS_GUI：双击时不弹控制台窗口。
    require(struct.unpack_from("<H", data, optional + 68)[0] == 2, "Not Windows GUI subsystem")
    resource_rva, resource_size = struct.unpack_from("<II", data, optional + 112 + 2 * 8)
    require(resource_rva and resource_size, "Resource directory missing")

    sections = []
    for index in range(count):
        offset = optional + optional_size + index * 40
        name, _, rva, size, file_offset = struct.unpack_from("<8sIIII", data, offset)
        # 用 SizeOfRawData 映射：VirtualSize 可能大于文件里实际存在的字节，
        # 会把下一个节区的范围也算进来。
        sections.append((name, rva, size, file_offset))

    def raw(rva, length):
        for _, start, size, file_offset in sections:
            if start <= rva and rva + length <= start + size:
                offset = file_offset + rva - start
                if offset + length <= len(data):
                    return offset
        raise ValueError("RVA outside file-backed sections")

    def resource(offset, length):
        require(offset + length <= resource_size, "Resource directory is truncated")
        return raw(resource_rva + offset, length)

    def entries(offset):
        _, _, _, _, named, ids = struct.unpack_from("<IIHHHH", data, resource(offset, 16))
        start = resource(offset + 16, (named + ids) * 8)
        return [
            struct.unpack_from("<II", data, start + index * 8)
            for index in range(named + ids)
        ]

    def child(offset, wanted):
        for identifier, location in entries(offset):
            if identifier == wanted:
                return location
        raise ValueError("Manifest resource entry missing")

    manifest_type = child(0, RT_MANIFEST)
    require(manifest_type & 0x80000000, "Manifest type is not a directory")
    manifest_name = child(manifest_type & 0x7FFFFFFF, MANIFEST_ID)
    require(manifest_name & 0x80000000, "Manifest name is not a directory")
    # 语言目录：资源编译器可能用 0、1033 或 2052，取实际存在的那一个。
    language_entries = entries(manifest_name & 0x7FFFFFFF)
    require(language_entries, "Manifest language entry missing")
    location = language_entries[0][1]
    require(not location & 0x80000000, "Manifest data entry is a directory")
    content_rva, size, _, _ = struct.unpack_from("<IIII", data, resource(location, 16))
    content = raw(content_rva, size)
    manifest = data[content : content + size]

    verify_manifest(manifest)
    print(
        "PE verified:",
        args.exe.name,
        args.arch,
        "GUI, embedded v6/asInvoker/DPI manifest",
    )


if __name__ == "__main__":
    main()
