"""Run packaging scripts against tiny Cargo projects and fake signing tools."""

import base64
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

from test_verify_pe import VALID_MANIFEST, synthetic_pe


SCRIPTS = Path(__file__).resolve().parent
RUST_TOOLCHAIN = SCRIPTS.parent / "rust-toolchain.toml"
WINDOWS_TARGET = "x86_64-pc-windows-msvc"

BUILD_RS = r'''
use std::{env, fs, path::PathBuf, process::Command};
fn main() {
    let output = Command::new(env::var_os("RUSTC").unwrap())
        .arg("--version").output().unwrap();
    let version = String::from_utf8(output.stdout).unwrap();
    println!("cargo:rustc-env=FIXTURE_RUSTC_VERSION={}", version.trim());
    let name = format!("{}-{}.txt", env::var("CARGO_PKG_NAME").unwrap(), env::var("TARGET").unwrap());
    let directory = PathBuf::from(env::var_os("OILFIND_TEST_TOOLCHAIN_LOG").unwrap());
    fs::write(directory.join(&name), &version).unwrap();
    println!("cargo:rerun-if-env-changed=OILFIND_TEST_TEMP_LOG");
    println!("cargo:rerun-if-env-changed=OILFIND_FORCE_BUILD_FAILURE");
    if let Some(log) = env::var_os("OILFIND_TEST_TEMP_LOG") {
        let temp = env::var("TEMP").unwrap_or_default();
        let tmp = env::var("TMP").unwrap_or_default();
        fs::write(PathBuf::from(log).join(name), format!("{temp}\n{tmp}\n")).unwrap();
    }
    if env::var_os("OILFIND_FORCE_BUILD_FAILURE").is_some() {
        eprintln!("intentional fixture build failure");
        std::process::exit(7);
    }
}
'''

FIXTURE_MAIN = r'''
fn main() { println!("{}", env!("FIXTURE_RUSTC_VERSION")); }

#[cfg(test)]
mod tests {
    #[test]
    fn project_temp_is_inherited() {
        use std::{env, fs, path::PathBuf};
        let Some(log) = env::var_os("OILFIND_TEST_TEMP_LOG") else { return };
        let temp = PathBuf::from(env::var_os("TEMP").unwrap());
        let tmp = PathBuf::from(env::var_os("TMP").unwrap());
        let project = PathBuf::from(env::var_os("OILFIND_FIXTURE_PROJECT").unwrap());
        assert_eq!(temp, tmp);
        assert_eq!(env::temp_dir(), temp);
        assert_eq!(temp.parent(), Some(project.as_path()));
        assert!(temp.is_dir());
        fs::write(PathBuf::from(log).join(format!("test-{}.txt", env!("CARGO_PKG_NAME"))),
                  format!("{}\n{}\n", temp.display(), tmp.display())).unwrap();
    }
}
'''

POWERSHELL_WRAPPER = r'''
$beforeTemp = [Environment]::GetEnvironmentVariable("TEMP", "Process")
$beforeTmp = [Environment]::GetEnvironmentVariable("TMP", "Process")
$success = $true
try {
    & $env:OILFIND_FIXTURE_SCRIPT -PythonBin $env:OILFIND_FIXTURE_PYTHON
}
catch {
    $success = $false
    [Console]::Error.WriteLine($_.ToString())
}
$result = @{
    beforeTemp = $beforeTemp
    beforeTmp = $beforeTmp
    afterTemp = [Environment]::GetEnvironmentVariable("TEMP", "Process")
    afterTmp = [Environment]::GetEnvironmentVariable("TMP", "Process")
    success = $success
}
[IO.File]::WriteAllText($env:OILFIND_TEST_POST_ENV, ($result | ConvertTo-Json -Compress))
if (-not $success) { exit 1 }
'''


def executable(path, content):
    path.write_text(content, encoding="utf-8")
    path.chmod(0o755)


def installed_toolchains():
    result = subprocess.run(
        ["rustup", "toolchain", "list"], capture_output=True, text=True, check=True
    )
    return [line.split()[0] for line in result.stdout.splitlines() if line.strip()]


def alternate_toolchain():
    for toolchain in installed_toolchains():
        result = subprocess.run(
            ["rustup", "run", toolchain, "rustc", "--version"],
            capture_output=True,
            text=True,
            check=True,
        )
        if not result.stdout.startswith("rustc 1.92.0 "):
            return toolchain
    return None


@unittest.skipUnless(shutil.which("cargo") and shutil.which("rustup"), "Rust is not installed")
class BuildScriptTests(unittest.TestCase):
    def setUp(self):
        if not any(name.startswith("1.92.0-") for name in installed_toolchains()):
            self.skipTest("the pinned Rust toolchain is not installed; no download is attempted")
        self.temporary = tempfile.TemporaryDirectory(prefix="oilfind-build-")
        self.addCleanup(self.temporary.cleanup)
        self.base = Path(self.temporary.name).resolve()
        self.project = self.base / "fixture project"
        self.rust = self.project / "rust"
        self.scripts = self.rust / "scripts"
        self.scripts.mkdir(parents=True)
        for name in ("build.sh", "build.ps1", "package-mac.sh", "verify-pe.py"):
            shutil.copy2(SCRIPTS / name, self.scripts / name)
        shutil.copy2(RUST_TOOLCHAIN, self.rust / "rust-toolchain.toml")
        (self.rust / "Cargo.toml").write_text(
            '[workspace]\nresolver = "2"\nmembers = ['
            '"crates/oilfind-mac", "crates/oilfind-win", "crates/oilfind-cli"]\n',
            encoding="utf-8",
        )
        for name in ("oilfind-mac", "oilfind-win", "oilfind-cli"):
            crate = self.rust / "crates" / name
            (crate / "src").mkdir(parents=True)
            (crate / "Cargo.toml").write_text(
                f'[package]\nname = "{name}"\nversion = "0.1.0"\nedition = "2021"\n',
                encoding="utf-8",
            )
            (crate / "build.rs").write_text(BUILD_RS, encoding="utf-8")
            (crate / "src" / "main.rs").write_text(FIXTURE_MAIN, encoding="utf-8")
        (self.rust / "crates" / "oilfind-mac" / "Info.plist").write_text("fixture")
        (self.project / "LICENSE").write_text("fixture license")
        (self.rust / "THIRD_PARTY_NOTICES.txt").write_text("fixture notices")
        (self.rust / "USAGE.txt").write_text("fixture usage")
        (self.rust / "LICENSES").mkdir()
        (self.rust / "LICENSES" / "license.txt").write_text("fixture license")

        self.caller = self.base / "different caller"
        self.caller.mkdir()
        self.alternate = alternate_toolchain()
        if self.alternate:
            (self.caller / "rust-toolchain.toml").write_text(
                f'[toolchain]\nchannel = "{self.alternate}"\n', encoding="utf-8"
            )
        self.bin = self.base / "stub commands"
        self.bin.mkdir()
        self.log = self.base / "compiler-logs"
        self.log.mkdir()
        self.stub_log = self.base / "stub.log"

    def environment(self, target):
        env = os.environ.copy()
        for name in (
            "RUSTUP_TOOLCHAIN",
            "RUSTC",
            "RUSTC_WRAPPER",
            "RUSTC_WORKSPACE_WRAPPER",
            "RUSTFLAGS",
            "CARGO_ENCODED_RUSTFLAGS",
            "CARGO_BUILD_TARGET",
            "PYTHONOPTIMIZE",
            "OILFIND_TEST_TEMP_LOG",
            "OILFIND_FORCE_BUILD_FAILURE",
        ):
            env.pop(name, None)
        env.update(
            CARGO_NET_OFFLINE="true",
            CARGO_TARGET_DIR=target,
            OILFIND_TEST_TOOLCHAIN_LOG=str(self.log),
            OILFIND_STUB_LOG=str(self.stub_log),
            PYTHONDONTWRITEBYTECODE="1",
            PYTHON_BIN=sys.executable,
            PATH=str(self.bin) + os.pathsep + env.get("PATH", ""),
        )
        return env

    def run_script(self, command, env):
        result = subprocess.run(
            command, cwd=self.caller, env=env, capture_output=True, text=True, timeout=180
        )
        self.assertEqual(result.returncode, 0, result.stdout + "\n" + result.stderr)
        versions = [path.read_text(encoding="utf-8").strip() for path in self.log.iterdir()]
        self.assertTrue(versions)
        self.assertTrue(all(line.startswith("rustc 1.92.0 ") for line in versions), versions)

    def run_powershell(self, env, expect_success):
        env["OILFIND_FIXTURE_SCRIPT"] = str(self.scripts / "build.ps1")
        env["OILFIND_FIXTURE_PYTHON"] = sys.executable
        env["OILFIND_TEST_POST_ENV"] = str(self.base / "environment-after.json")
        encoded = base64.b64encode(POWERSHELL_WRAPPER.encode("utf-16le")).decode("ascii")
        result = subprocess.run(
            ["pwsh", "-NoLogo", "-NoProfile", "-NonInteractive", "-EncodedCommand", encoded],
            cwd=self.caller, env=env, capture_output=True, text=True, timeout=180,
        )
        self.assertEqual(result.returncode, 0 if expect_success else 1, result.stdout + result.stderr)
        after = json.loads(Path(env["OILFIND_TEST_POST_ENV"]).read_text(encoding="utf-8-sig"))
        self.assertEqual(after["success"], expect_success)
        self.assertEqual(after["beforeTemp"], env.get("TEMP"))
        self.assertEqual(after["beforeTmp"], env.get("TMP"))
        self.assertEqual(after["afterTemp"], after["beforeTemp"])
        self.assertEqual(after["afterTmp"], after["beforeTmp"])
        versions = [path.read_text(encoding="utf-8").strip() for path in self.log.iterdir()]
        self.assertTrue(versions)
        self.assertTrue(all(line.startswith("rustc 1.92.0 ") for line in versions), versions)

        temp_logs = list(Path(env["OILFIND_TEST_TEMP_LOG"]).iterdir())
        self.assertTrue(temp_logs)
        if expect_success:
            self.assertTrue(any(path.name.startswith("test-") for path in temp_logs))
            self.assertTrue(any(not path.name.startswith("test-") for path in temp_logs))
        used = set()
        for path in temp_logs:
            temp, tmp = path.read_text(encoding="utf-8").splitlines()
            self.assertEqual(temp, tmp)
            directory = Path(temp)
            self.assertEqual(directory.parent.resolve(), self.project.resolve())
            self.assertTrue(directory.name.startswith(".oilfind-build-temp-"))
            self.assertFalse(directory.exists())
            used.add(directory)
        self.assertEqual(len(used), 1, used)
        return used.pop()

    def target_cases(self):
        return (
            (str(self.base / "custom cargo target"), self.base / "custom cargo target"),
            ("relative cargo target", self.rust / "relative cargo target"),
        )

    @unittest.skipUnless(os.name == "posix", "shell packaging requires POSIX")
    def test_mac_package_uses_real_cargo_target_and_pinned_toolchain(self):
        executable(
            self.bin / "codesign",
            '#!/bin/sh\nprintf "codesign\\n" >> "$OILFIND_STUB_LOG"\n',
        )
        executable(
            self.bin / "ditto",
            '#!/bin/sh\nprintf "ditto\\n" >> "$OILFIND_STUB_LOG"\n'
            'printf "fixture archive\\n" > "$5"\n',
        )
        stale = self.rust / "target" / "release" / "oilfind-mac"
        stale.parent.mkdir(parents=True)
        stale.write_bytes(b"old binary must not be packaged")
        for configured, target in self.target_cases():
            with self.subTest(target=configured):
                self.run_script(["bash", str(self.scripts / "package-mac.sh")], self.environment(configured))
                built = list(target.rglob("oilfind-mac"))
                built = [path for path in built if path.parent.name == "release"]
                self.assertEqual(len(built), 1, built)
                packaged = self.project / "build" / "Oil Find Rust.app" / "Contents" / "MacOS" / "oilfind-mac"
                self.assertEqual(packaged.read_bytes(), built[0].read_bytes())
                self.assertNotEqual(packaged.read_bytes(), stale.read_bytes())
                version = subprocess.run([str(packaged)], capture_output=True, text=True, check=True)
                self.assertTrue(version.stdout.startswith("rustc 1.92.0 "), version.stdout)
                self.assertTrue((self.project / "outputs" / "OilFind-Rust-macOS-preview.zip").is_file())
        self.assertEqual(self.stub_log.read_text().splitlines(), ["codesign", "ditto"] * 2)

    @unittest.skipUnless(os.name == "posix", "cross-build smoke test requires POSIX")
    def test_windows_cross_build_uses_configured_target_with_real_cargo(self):
        installed = subprocess.run(
            ["rustup", "target", "list", "--installed", "--toolchain", "1.92.0"],
            capture_output=True, text=True, check=True,
        ).stdout.splitlines()
        if WINDOWS_TARGET not in installed:
            self.skipTest("the pinned Windows target is not installed; no download is attempted")
        pe = self.base / "synthetic.exe"
        pe.write_bytes(synthetic_pe())
        linker = self.bin / "fixture-linker"
        executable(
            linker,
            '#!/bin/sh\nout=\nfor arg in "$@"; do\n'
            '  case "$arg" in /OUT:*|/out:*|-OUT:*|-out:*) out="${arg#*:}" ;; esac\n'
            'done\n[ -n "$out" ] || exit 1\ncp "$OILFIND_FIXTURE_PE" "$out"\n',
        )
        stale = self.rust / "target" / WINDOWS_TARGET / "release"
        stale.mkdir(parents=True)
        stale_manifest = VALID_MANIFEST.replace(b"<assembly ", b'<assembly fixture="stale" ', 1)
        (stale / "oilfind-win.exe").write_bytes(synthetic_pe(manifest=stale_manifest))
        (stale / "oilfind-cli.exe").write_bytes(b"stale cli")
        for configured, target in self.target_cases():
            with self.subTest(target=configured):
                env = self.environment(configured)
                env["CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER"] = str(linker)
                env["OILFIND_FIXTURE_PE"] = str(pe)
                self.run_script(["bash", str(self.scripts / "build.sh")], env)
                release = target / WINDOWS_TARGET / "release"
                output = self.project / "outputs" / "windows-amd64"
                self.assertEqual(sorted(path.name for path in output.iterdir()), ["OilFind.exe"])
                self.assertEqual((output / "OilFind.exe").read_bytes(), (release / "oilfind-win.exe").read_bytes())
                self.assertNotEqual((output / "OilFind.exe").read_bytes(), (stale / "oilfind-win.exe").read_bytes())

    @unittest.skipUnless(os.name == "nt" and shutil.which("pwsh"), "Windows PowerShell is unavailable")
    def test_windows_powershell_uses_configured_target_with_real_cargo(self):
        # This tiny fixture has no resource compiler; PE validation itself is
        # exercised separately by test_verify_pe.py.
        (self.scripts / "verify-pe.py").write_text(
            'import pathlib, sys\nsys.exit(0 if pathlib.Path(sys.argv[-1]).read_bytes()[:2] == b"MZ" else 1)\n',
            encoding="utf-8",
        )
        stale = self.rust / "target" / "release"
        stale.mkdir(parents=True)
        (stale / "oilfind-win.exe").write_bytes(b"stale gui")
        (stale / "oilfind-cli.exe").write_bytes(b"stale cli")
        existing = self.project / ".oilfind-build-temp-existing"
        existing.mkdir()
        sentinel = existing / "keep.txt"
        sentinel.write_text("do not delete")
        original_temp = self.base / "original TEMP"
        original_tmp = self.base / "original TMP"
        original_temp.mkdir()
        original_tmp.mkdir()
        used = set()
        for number, (configured, target) in enumerate(self.target_cases()):
            with self.subTest(target=configured):
                log = self.base / f"temp-logs-{number}"
                log.mkdir()
                env = self.environment(configured)
                env.update(
                    TEMP=str(original_temp), TMP=str(original_tmp),
                    OILFIND_TEST_TEMP_LOG=str(log), OILFIND_FIXTURE_PROJECT=str(self.project),
                )
                used.add(self.run_powershell(env, expect_success=True))
                gui = [p for p in target.rglob("oilfind-win.exe") if p.parent.name == "release"]
                self.assertEqual(len(gui), 1, gui)
                output = self.project / "outputs" / "windows-amd64"
                self.assertEqual(sorted(path.name for path in output.iterdir()), ["OilFind.exe"])
                self.assertEqual((output / "OilFind.exe").read_bytes(), gui[0].read_bytes())
        self.assertEqual(len(used), 2, used)
        self.assertEqual(sentinel.read_text(), "do not delete")
        self.assertEqual(set(self.project.glob(".oilfind-build-temp-*")), {existing})

    @unittest.skipUnless(os.name == "nt" and shutil.which("pwsh"), "Windows PowerShell is unavailable")
    def test_windows_powershell_restores_temp_after_cargo_failure(self):
        existing = self.project / ".oilfind-build-temp-existing"
        existing.mkdir()
        sentinel = existing / "keep.txt"
        sentinel.write_text("do not delete")
        original_temp = self.base / "original TEMP"
        original_temp.mkdir()
        original_marker = original_temp / "keep.txt"
        original_marker.write_text("keep original TEMP")
        log = self.base / "failure-temp-logs"
        log.mkdir()
        env = self.environment(str(self.base / "failure cargo target"))
        env.update(
            TEMP=str(original_temp), OILFIND_TEST_TEMP_LOG=str(log),
            OILFIND_FIXTURE_PROJECT=str(self.project), OILFIND_FORCE_BUILD_FAILURE="1",
        )
        env.pop("TMP", None)
        self.run_powershell(env, expect_success=False)
        self.assertEqual(sentinel.read_text(), "do not delete")
        self.assertEqual(original_marker.read_text(), "keep original TEMP")
        self.assertEqual(set(self.project.glob(".oilfind-build-temp-*")), {existing})


if __name__ == "__main__":
    unittest.main()
