from pathlib import Path
import plistlib
import subprocess
import unittest


ROOT = Path(__file__).resolve().parents[2]
MODULE = "OilFind" + "Pro"


class OpenCoreTests(unittest.TestCase):
    def test_private_repository_is_not_tracked(self):
        files = subprocess.check_output(["git", "ls-files", "-z"], cwd=ROOT).decode().split("\0")
        self.assertFalse([name for name in files if name.startswith("Pro/")])
        ignored = subprocess.run(["git", "check-ignore", "Pro/Sources/placeholder.swift"], cwd=ROOT,
                                 capture_output=True, text=True)
        self.assertEqual(ignored.returncode, 0, ignored.stderr)

    def test_fork_identity_is_distinct_from_upstream(self):
        with (ROOT / "Resources/Info.plist").open("rb") as source:
            info = plistlib.load(source)
        self.assertEqual(info["CFBundleIdentifier"], "com.reasonix.oilfind.intranet")
        self.assertEqual(info["CFBundleDisplayName"], "Oil Find Intranet")
        self.assertEqual(info["CFBundleExecutable"], "OilFindIntranet")
        self.assertTrue(info["OilFindIntranetManaged"])
        self.assertEqual(info["CFBundleURLTypes"][0]["CFBundleURLSchemes"], ["oilfind-intranet"])
        app = (ROOT / "Sources/OilFindApp/Application.swift").read_text()
        self.assertIn('preferenceDomain = "com.reasonix.oilfind.intranet"', app)
        self.assertIn("Library/Application Support/Oil Find Intranet", app)
        self.assertIn("Library/Caches/Oil Find Intranet", app)
        cli = (ROOT / "Sources/oilfind-cli/main.swift").read_text()
        watch = cli.split('case "watch":', 1)[1].split('case "m2-bench":', 1)[0]
        self.assertIn("IndexManager(config: config, dbURL: URL(fileURLWithPath: db), settingsDefaults: nil)", watch)
        sources = "\n".join(path.read_text() for path in (ROOT / "Sources/OilFindApp").rglob("*.swift"))
        self.assertNotIn("LegacyAuthorizationCleanup", sources)
        self.assertNotIn("SecItemDelete", sources)
        self.assertNotIn("UserDefaults.standard", sources)
        self.assertNotIn("UserDefaults = .standard", sources)
        self.assertNotIn("Library/Application Support/Oil Find/", sources)
        self.assertNotIn("Library/Caches/Oil Find/", sources)
        for script in ("install.sh", "uninstall.sh", "build-app.sh"):
            self.assertNotIn("com.oiloil.find", (ROOT / "scripts" / script).read_text())
        install = (ROOT / "scripts/install.sh").read_text()
        build = (ROOT / "scripts/build-app.sh").read_text()
        self.assertIn("unset OILFIND_SIGN_IDENTITY", install)
        self.assertIn('--sign "$OILFIND_SIGN_IDENTITY" --identifier "$BUNDLE_ID"', build)
        self.assertIn("--timestamp=none", build)
        self.assertNotIn("security find-identity", build)
        package = (ROOT / "scripts/package.sh").read_text()
        self.assertIn("Oil Find Intranet.app", package)
        self.assertIn("Oil-Find-Intranet-$VERSION.zip", package)

    def test_exclusion_and_offline_guards_are_wired_before_search_or_launch(self):
        app = (ROOT / "Sources/OilFindApp/Application.swift").read_text()
        self.assertLess(app.index("if UpdateExercise.handles(arguments)"), app.index("appExtension?.handleLaunch"))
        self.assertIn("#if !OILFIND_OFFLINE_BUILD\n    SystemUpdateFiles().registerLaunch", app)
        delegate = (ROOT / "Sources/OilFindApp/AppDelegate.swift").read_text()
        self.assertIn("#if !OILFIND_OFFLINE_BUILD\n        if let failure = SystemUpdateFiles().cleanupAfterLaunch", delegate)
        config_change = delegate.split("model.onConfigChange =", 1)[1].split("model.onPinyinChange =", 1)[0]
        self.assertIn("manager?.updateConfig", config_change)
        self.assertIn("externalDrives.updateSettings()", config_change)
        self.assertLess(config_change.index("searchController.invalidateIndexConfiguration()"), config_change.index("externalDrives.updateSettings()"))
        self.assertIn("searchController.startSearch()", config_change)
        self.assertIn("model.onExcludedFoldersSelected", delegate)
        self.assertIn("filteredExtensionSources", delegate)
        settings = (ROOT / "Sources/OilFindApp/SettingsWindow.swift").read_text()
        self.assertIn("onExcludedFoldersSelected?(selected)", settings)
        self.assertIn("else if bindingChanged { self.onConfigChange?()", settings)
        controller = (ROOT / "Sources/OilFindApp/SearchViewController.swift").read_text()
        invalidation = controller.split("func invalidateIndexConfiguration()", 1)[1].split("func indexChanged()", 1)[0]
        self.assertIn("generation.next()", invalidation)
        self.assertIn("results.clear()", invalidation)
        self.assertIn("quickLook.close()", invalidation)

    def test_public_module_boundary(self):
        allowed = {"Package.swift", "Sources/OilFind/main.swift"}
        # Include new source files before they have been staged. Never inspect secrets or build products.
        paths = [ROOT / "Package.swift"]
        for directory in ("Sources", "Tests", "scripts", ".github", "Resources"):
            paths.extend(path for path in (ROOT / directory).rglob("*")
                         if path.is_file() and not path.name.startswith(".env"))
        offenders = []
        for path in paths:
            if path.relative_to(ROOT).as_posix() in allowed:
                continue
            try:
                if MODULE in path.read_text():
                    offenders.append(path.relative_to(ROOT).as_posix())
            except UnicodeDecodeError:
                continue
        self.assertEqual(offenders, [])


if __name__ == "__main__":
    unittest.main()
