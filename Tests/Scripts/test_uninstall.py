import os
from pathlib import Path
import plistlib
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
BUNDLE_ID = "com.reasonix.oilfind.intranet"
EXECUTABLE = "OilFindIntranet"


class IsolatedInstallationTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="oilfind-script-tests-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.log = self.root / "calls.bin"
        stubs = self.root / "stubs"
        stubs.mkdir()
        stub = stubs / "stub"
        stub.write_text("""#!/bin/bash
printf '%s\\0' "${0##*/}" "$@" >> "$OILFIND_TEST_LOG"
printf '\\0' >> "$OILFIND_TEST_LOG"
case "${0##*/}" in
    pgrep)
        if [[ "${OILFIND_UPSTREAM_RUNNING:-0}" == 1 && "$1" == -x && "$2" == OilFind ]]; then printf '700\\n'; exit 0; fi
        status="${OILFIND_PGREP_STATUS:-1}"
        if [[ "$status" == 0 ]]; then printf '701\\n'; fi
        exit "$status" ;;
    ps) printf '%s\\n' "${OILFIND_PS_COMMAND:-}"; exit "${OILFIND_PS_STATUS:-0}" ;;
    swift)
        if [[ " $* " == *" --show-bin-path "* ]]; then printf '%s\\n' "$OILFIND_TEST_BIN"; fi
        exit 0 ;;
    rm|mkdir|ditto|cp|strip|codesign|python3) exit 0 ;;
    *) exit 98 ;;
esac
""")
        stub.chmod(0o755)
        for name in ("pgrep", "ps", "rm", "mkdir", "ditto", "cp", "strip", "swift", "codesign", "python3",
                     "defaults", "security", "osascript", "open", "kill", "pkill"):
            (stubs / name).symlink_to(stub)
        self.env = dict(os.environ, PATH=str(stubs) + ":/usr/bin:/bin", HOME=str(self.root / "home"),
                        OILFIND_TEST_LOG=str(self.log), OILFIND_TEST_BIN=str(self.root / "bin"))
        self.env.pop("OILFIND_SIGN_IDENTITY", None)

    def bundle(self, name, bundle_id=BUNDLE_ID, marker=True):
        app = self.root / name
        executable = app / "Contents/MacOS" / EXECUTABLE
        executable.parent.mkdir(parents=True)
        executable.write_bytes(b"isolated test executable")
        executable.chmod(0o755)
        with (app / "Contents/Info.plist").open("wb") as output:
            plistlib.dump({"CFBundleIdentifier": bundle_id, "CFBundleExecutable": EXECUTABLE,
                           "CFBundlePackageType": "APPL", "OilFindIntranetManaged": marker}, output)
        return app

    def invoke(self, script, function, *arguments, answer="y\n", pgrep_status=1,
               ps_command="", ps_status=0, upstream_running=False):
        return subprocess.run(["/bin/bash", "-c", 'source "$1"; shift; "$@"', "test",
                               str(ROOT / "scripts" / script), function, *map(str, arguments)],
                              input=answer, text=True, capture_output=True,
                              env=dict(self.env, OILFIND_PGREP_STATUS=str(pgrep_status),
                                       OILFIND_PS_COMMAND=ps_command, OILFIND_PS_STATUS=str(ps_status),
                                       OILFIND_UPSTREAM_RUNNING="1" if upstream_running else "0"))

    def calls(self):
        if not self.log.exists():
            return []
        return [[part.decode() for part in call.split(b"\0")]
                for call in self.log.read_bytes().split(b"\0\0") if call]

    def test_uninstall_refuses_other_apps_and_symlinks(self):
        foreign = self.bundle("foreign.app", bundle_id="com.oiloil.find")
        result = self.invoke("uninstall.sh", "uninstall_app", foreign)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Refusing", result.stderr)
        link = self.root / "link.app"
        link.symlink_to(self.bundle("owned.app"), target_is_directory=True)
        result = self.invoke("uninstall.sh", "uninstall_app", link)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.calls(), [])

    def test_uninstall_only_removes_confirmed_isolated_app(self):
        app = self.bundle("Oil Find Intranet.app")
        result = self.invoke("uninstall.sh", "uninstall_app", app, answer="n\n")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.calls(), [["pgrep", "-x", EXECUTABLE]])
        self.log.unlink()
        result = self.invoke("uninstall.sh", "uninstall_app", app)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("retained", result.stdout)
        self.assertEqual(self.calls(), [["pgrep", "-x", EXECUTABLE], ["rm", "-rf", "--", str(app)]])
        self.assertTrue(app.exists(), "The stub must never delete even the temporary fixture")

    def test_uninstall_refuses_running_target_or_unknown_process_state(self):
        app = self.bundle("running.app")
        executable = app / "Contents/MacOS" / EXECUTABLE
        result = self.invoke("uninstall.sh", "uninstall_app", app, pgrep_status=0,
                             ps_command=str(executable) + " --example")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.calls(), [["pgrep", "-x", EXECUTABLE], ["ps", "-p", "701", "-o", "command="]])
        self.log.unlink()
        result = self.invoke("uninstall.sh", "uninstall_app", app, pgrep_status=2)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.calls(), [["pgrep", "-x", EXECUTABLE]])
        self.log.unlink()
        result = self.invoke("uninstall.sh", "uninstall_app", app, pgrep_status=0, ps_status=2)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.calls(), [["pgrep", "-x", EXECUTABLE], ["ps", "-p", "701", "-o", "command="]])

    def test_unrelated_process_with_same_executable_name_is_ignored(self):
        app = self.bundle("owned.app")
        unrelated = str(self.root / "another.app/Contents/MacOS" / EXECUTABLE)
        result = self.invoke("uninstall.sh", "uninstall_app", app, pgrep_status=0, ps_command=unrelated)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.calls(), [["pgrep", "-x", EXECUTABLE], ["ps", "-p", "701", "-o", "command="],
                                        ["rm", "-rf", "--", str(app)]])

    def test_upstream_running_without_isolated_target_does_not_block_or_quit_it(self):
        source = self.bundle("source.app")
        target = self.root / "Oil Find Intranet.app"
        result = self.invoke("uninstall.sh", "uninstall_app", target, upstream_running=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.calls(), [])
        result = self.invoke("install.sh", "install_app", source, target, upstream_running=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.calls(), [["mkdir", str(target)],
                                        ["ditto", str(source / "Contents"), str(target / "Contents")]])

    def test_upstream_running_does_not_block_isolated_uninstall(self):
        app = self.bundle("owned.app")
        result = self.invoke("uninstall.sh", "uninstall_app", app, upstream_running=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.calls(), [["pgrep", "-x", EXECUTABLE], ["rm", "-rf", "--", str(app)]])

    def test_purge_is_explicitly_disabled(self):
        result = subprocess.run(["/bin/bash", str(ROOT / "scripts/uninstall.sh"), "--purge"],
                                input="y\n", text=True, capture_output=True, env=self.env)
        self.assertEqual(result.returncode, 2)
        self.assertIn("--purge is disabled", result.stderr)
        self.assertEqual(self.calls(), [])

    def test_install_refuses_all_existing_targets(self):
        source = self.bundle("source.app")
        foreign = self.bundle("other.app", bundle_id="com.oiloil.find")
        existing = self.bundle("existing.app")
        linked = self.root / "linked.app"
        linked.symlink_to(existing, target_is_directory=True)
        for target in (foreign, existing, linked):
            result = self.invoke("install.sh", "install_app", source, target)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("Refusing", result.stderr)
            self.assertEqual(self.calls(), [])

    def test_install_claims_only_new_isolated_target(self):
        source = self.bundle("source.app")
        target = self.root / "Oil Find Intranet.app"
        result = self.invoke("install.sh", "install_app", source, target)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.calls(), [["mkdir", str(target)],
                                        ["ditto", str(source / "Contents"), str(target / "Contents")]])
        self.assertFalse(target.exists(), "The stub must not perform an installation")

    def test_install_rejects_unrecognized_source(self):
        source = self.bundle("source.app", marker=False)
        result = self.invoke("install.sh", "install_app", source, self.root / "destination.app")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.calls(), [])

    def test_build_signing_selection_is_stubbed_and_uses_fork_identity(self):
        scripts = self.root / "project/scripts"
        scripts.mkdir(parents=True)
        for name in ("build-app.sh", "install.sh"):
            (scripts / name).write_text((ROOT / "scripts" / name).read_text())
        for identity in (None, "Isolated Test Signing Identity"):
            self.log.unlink(missing_ok=True)
            env = dict(self.env)
            if identity is not None:
                env["OILFIND_SIGN_IDENTITY"] = identity
            result = subprocess.run(["/bin/bash", str(scripts / "build-app.sh")], env=env,
                                    text=True, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            calls = self.calls()
            signing = [call for call in calls if call[0] == "codesign"]
            self.assertEqual(len(signing), 1)
            self.assertIn(BUNDLE_ID, signing[0])
            self.assertEqual(signing[0][signing[0].index("--sign") + 1], identity or "-")
            self.assertEqual("--timestamp=none" in signing[0], identity is not None)
            self.assertFalse(any(call[0] in ("security", "open", "osascript") for call in calls))


if __name__ == "__main__":
    unittest.main()
