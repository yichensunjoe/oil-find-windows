"""Exercise PE verification with tiny, isolated synthetic executables."""

import os
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import unittest


VERIFY = Path(__file__).with_name("verify-pe.py")
VALID_MANIFEST = (
    b'<assembly xmlns="urn:schemas-microsoft-com:asm.v1">'
    b'<dependency><dependentAssembly><assemblyIdentity '
    b'name="Microsoft.Windows.Common-Controls" version="6.0.0.0"/>'
    b'</dependentAssembly></dependency>'
    b'<trustInfo xmlns="urn:schemas-microsoft-com:asm.v3"><security>'
    b'<requestedPrivileges><requestedExecutionLevel level="asInvoker"/>'
    b'</requestedPrivileges></security></trustInfo>'
    b'<application xmlns="urn:schemas-microsoft-com:asm.v3"><windowsSettings>'
    b'<dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">'
    b'PerMonitorV2</dpiAwareness></windowsSettings></application></assembly>'
)


def synthetic_pe(manifest=VALID_MANIFEST, machine=0x8664, subsystem=2):
    """Build just enough PE32+ headers and resources for the verifier."""
    raw_size = max(0x400, (0x80 + len(manifest) + 0x1FF) & ~0x1FF)
    data = bytearray(0x200 + raw_size)
    pe, optional, section, resource = 0x80, 0x98, 0x188, 0x200
    data[:2] = b"MZ"
    struct.pack_into("<I", data, 0x3C, pe)
    data[pe : pe + 4] = b"PE\0\0"
    struct.pack_into("<HH", data, pe + 4, machine, 1)
    struct.pack_into("<H", data, pe + 20, 0xF0)
    struct.pack_into("<H", data, optional, 0x20B)
    struct.pack_into("<H", data, optional + 68, subsystem)
    struct.pack_into("<II", data, optional + 112 + 2 * 8, 0x1000, raw_size)
    struct.pack_into("<8sIIII", data, section, b".rsrc", raw_size, 0x1000, raw_size, resource)

    for offset in (0, 0x18, 0x30):
        struct.pack_into("<IIHHHH", data, resource + offset, 0, 0, 0, 0, 0, 1)
    struct.pack_into("<II", data, resource + 0x10, 24, 0x80000018)
    struct.pack_into("<II", data, resource + 0x28, 1, 0x80000030)
    struct.pack_into("<II", data, resource + 0x40, 1033, 0x48)
    struct.pack_into("<IIII", data, resource + 0x48, 0x1080, len(manifest), 0, 0)
    data[resource + 0x80 : resource + 0x80 + len(manifest)] = manifest
    return bytes(data)


def patched_pe(offset, replacement):
    data = bytearray(synthetic_pe())
    data[offset : offset + len(replacement)] = replacement
    return bytes(data)


def run_verifier(path, arch, mode):
    env = os.environ.copy()
    env.pop("PYTHONOPTIMIZE", None)
    command = [sys.executable]
    if mode == "flag":
        command.append("-O")
    elif mode == "environment":
        env["PYTHONOPTIMIZE"] = "1"
    command.extend([str(VERIFY), "--arch", arch, str(path)])
    return subprocess.run(command, capture_output=True, text=True, env=env, timeout=15)


class VerifyPeTests(unittest.TestCase):
    def test_valid_architectures_in_all_modes(self):
        with tempfile.TemporaryDirectory(prefix="oilfind-pe-") as temporary:
            path = Path(temporary) / "fixture.exe"
            for arch, machine in (("amd64", 0x8664), ("arm64", 0xAA64)):
                path.write_bytes(synthetic_pe(machine=machine))
                for mode in ("normal", "flag", "environment"):
                    with self.subTest(arch=arch, mode=mode):
                        result = run_verifier(path, arch, mode)
                        self.assertEqual(result.returncode, 0, result.stderr)

    def test_utf8_and_utf16_resource_padding(self):
        utf16 = VALID_MANIFEST.decode("utf-8").encode("utf-16")
        manifests = (VALID_MANIFEST + b"\0", utf16, utf16 + b"\0\0")
        with tempfile.TemporaryDirectory(prefix="oilfind-pe-") as temporary:
            path = Path(temporary) / "fixture.exe"
            for manifest in manifests:
                path.write_bytes(synthetic_pe(manifest))
                for mode in ("normal", "flag", "environment"):
                    with self.subTest(size=len(manifest), mode=mode):
                        result = run_verifier(path, "amd64", mode)
                        self.assertEqual(result.returncode, 0, result.stderr)

    def test_amd64_is_rejected_when_arm64_is_requested(self):
        with tempfile.TemporaryDirectory(prefix="oilfind-pe-") as temporary:
            path = Path(temporary) / "fixture.exe"
            path.write_bytes(synthetic_pe(machine=0x8664))
            for mode in ("normal", "flag", "environment"):
                with self.subTest(mode=mode):
                    result = run_verifier(path, "arm64", mode)
                    self.assertNotEqual(result.returncode, 0, result.stdout)
                    self.assertIn("Wrong architecture", result.stderr)

    def test_project_manifest_in_a_synthetic_pe_is_accepted(self):
        manifest = (VERIFY.parent.parent / "assets" / "app.manifest").read_bytes()
        with tempfile.TemporaryDirectory(prefix="oilfind-pe-") as temporary:
            path = Path(temporary) / "fixture.exe"
            path.write_bytes(synthetic_pe(manifest))
            for mode in ("normal", "flag", "environment"):
                with self.subTest(mode=mode):
                    result = run_verifier(path, "amd64", mode)
                    self.assertEqual(result.returncode, 0, result.stderr)

    def test_invalid_headers_and_manifests_fail_in_all_modes(self):
        invalid = {
            "missing DOS signature": (patched_pe(0, b"XX"), "Missing DOS signature"),
            "missing PE signature": (patched_pe(0x80, b"XX"), "Missing PE signature"),
            "wrong architecture": (synthetic_pe(machine=0xAA64), "Wrong architecture"),
            "32-bit optional header": (patched_pe(0x98, b"\x0b\x01"), "Not a 64-bit PE"),
            "console subsystem": (synthetic_pe(subsystem=3), "Not Windows GUI subsystem"),
            "no resource directory": (patched_pe(0x98 + 112 + 2 * 8, b"\0" * 8), "Resource directory missing"),
            "no embedded manifest": (patched_pe(0x210, b"\x19\0\0\0"), "Manifest resource entry missing"),
            "missing asInvoker": (
                synthetic_pe(VALID_MANIFEST.replace(b"asInvoker", b"unknown")),
                "asInvoker",
            ),
            "requires elevation": (
                synthetic_pe(VALID_MANIFEST.replace(b"asInvoker", b"requireAdministrator")),
                "elevation",
            ),
            "spoofed asInvoker comment": (
                synthetic_pe(
                    VALID_MANIFEST.replace(b'level="asInvoker"', b'level="highestAvailable"')
                    .replace(b"</assembly>", b"<!-- asInvoker --></assembly>")
                ),
                "asInvoker",
            ),
            "missing common controls": (
                synthetic_pe(VALID_MANIFEST.replace(b"Microsoft.Windows.Common-Controls", b"other")),
                "Common-Controls",
            ),
            "wrong common controls version": (
                synthetic_pe(VALID_MANIFEST.replace(b"6.0.0.0", b"5.0.0.0")),
                "Common-Controls",
            ),
            "malformed xml": (synthetic_pe(VALID_MANIFEST[:-1]), "valid XML"),
            "missing dpi awareness": (
                synthetic_pe(VALID_MANIFEST.replace(b"PerMonitorV2", b"other")),
                "PerMonitorV2",
            ),
        }
        with tempfile.TemporaryDirectory(prefix="oilfind-pe-") as temporary:
            path = Path(temporary) / "fixture.exe"
            for label, (data, error) in invalid.items():
                path.write_bytes(data)
                for mode in ("normal", "flag", "environment"):
                    with self.subTest(case=label, mode=mode):
                        result = run_verifier(path, "amd64", mode)
                        self.assertNotEqual(result.returncode, 0, result.stdout)
                        self.assertIn(error, result.stderr)

    def test_truncated_resource_is_rejected(self):
        data = bytearray(synthetic_pe())
        # Point the manifest at the last byte while claiming a full document.
        raw_size = struct.unpack_from("<I", data, 0x98 + 112 + 2 * 8 + 4)[0]
        struct.pack_into("<IIII", data, 0x248, 0x1000 + raw_size - 1, len(VALID_MANIFEST), 0, 0)
        with tempfile.TemporaryDirectory(prefix="oilfind-pe-") as temporary:
            path = Path(temporary) / "fixture.exe"
            path.write_bytes(data)
            for mode in ("normal", "flag", "environment"):
                with self.subTest(mode=mode):
                    result = run_verifier(path, "amd64", mode)
                    self.assertNotEqual(result.returncode, 0, result.stdout)


if __name__ == "__main__":
    unittest.main()
