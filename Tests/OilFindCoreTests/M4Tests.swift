import XCTest
import Carbon
import Darwin
@testable import OilFindCore

final class M4Tests: XCTestCase {
    func testT50ShortcutSymbols() {
        let modifiers = [controlKey, optionKey, shiftKey, cmdKey]
        let symbols = ["⌃", "⌥", "⇧", "⌘"]
        for combination in 0..<16 {
            var mask: UInt32 = 0, prefix = ""
            for bit in 0..<4 where combination & (1 << bit) != 0 {
                mask |= UInt32(modifiers[bit]); prefix += symbols[bit]
            }
            XCTAssertEqual(Shortcut.symbols(keyCode: UInt32(kVK_ANSI_F), modifiers: mask), prefix + "F")
        }
        XCTAssertEqual(Shortcut.symbols(keyCode: Shortcut.defaultKeyCode, modifiers: Shortcut.defaultModifiers), "⌃⌥⇧F")
        for (code, name) in [(kVK_ANSI_A, "A"), (kVK_ANSI_Z, "Z"), (kVK_ANSI_0, "0"), (kVK_ANSI_9, "9"),
            (kVK_F1, "F1"), (kVK_F12, "F12"), (kVK_F20, "F20"), (kVK_LeftArrow, "←"), (kVK_RightArrow, "→"),
            (kVK_UpArrow, "↑"), (kVK_DownArrow, "↓"), (kVK_Space, "␣"), (kVK_Return, "↩"), (kVK_ANSI_KeypadEnter, "⌤"),
            (kVK_Tab, "⇥"), (kVK_Delete, "⌫"), (kVK_ForwardDelete, "⌦"), (kVK_Escape, "⎋"),
            (kVK_Home, "↖"), (kVK_End, "↘"), (kVK_PageUp, "⇞"), (kVK_PageDown, "⇟"), (kVK_ANSI_KeypadClear, "⌧")] {
            XCTAssertEqual(Shortcut.symbols(keyCode: UInt32(code), modifiers: UInt32(cmdKey)), "⌘" + name)
        }
        XCTAssertEqual(Shortcut.keyEquivalent(UInt32(kVK_F1)), "\u{f704}")
        XCTAssertEqual(Shortcut.keyEquivalent(UInt32(kVK_Space)), " ")
        XCTAssertEqual(Shortcut.keyEquivalent(UInt32(kVK_Return)), "\r")
        XCTAssertEqual(Shortcut.keyEquivalent(UInt32(kVK_Home)), "\u{f729}")
        XCTAssertEqual(Shortcut.keyEquivalent(UInt32(kVK_ForwardDelete)), "\u{f728}")
    }

    func testT51ExcludedFolders() throws {
        XCTAssertTrue(ExcludedFolders.contains("/Users/me/Documents", path: "/Users/me/Documents/a/b"))
        XCTAssertTrue(ExcludedFolders.contains("/Users/me/Documents/", path: "/Users/me/Documents"))
        XCTAssertFalse(ExcludedFolders.contains("/Users/me/Documents", path: "/Users/me/Documents-old/a"))
        XCTAssertFalse(ExcludedFolders.contains("/Users/me/Documents/a", path: "/Users/me/Documents"))
        XCTAssertTrue(ExcludedFolders.contains("/", path: "/Users/me"))
        XCTAssertNil(ExcludedFolders.normalized("relative"))
        XCTAssertEqual(ExcludedFolders.normalized("/cafe\u{301}/"), "/café")
        let unicodeRule = IndexConfig(userExcludedPaths: ExcludedFolders.adding(["/cafe\u{301}"], to: []))
        Array("/café/leaf".utf8).withUnsafeBufferPointer { XCTAssertTrue(unicodeRule.isExcluded(path: $0)) }
        XCTAssertEqual(ExcludedFolders.adding(["/a/", "/a/b", "/a", "/abc", "relative", "/a/../c"], to: []), ["/a", "/abc", "/c"])
        XCTAssertEqual(ExcludedFolders.adding(["/a", "/a/d"], to: ["/a/b", "/a/c", "/other"]), ["/other", "/a"])
        XCTAssertEqual(ExcludedFolders.adding(["/"], to: ["/a", "/b"]), ["/"])
        Array("/Users/me/file".utf8).withUnsafeBufferPointer {
            XCTAssertTrue(IndexConfig(userExcludedPaths: ["/"]).isExcluded(path: $0))
        }
        let suite = "OilFindM4-\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        SettingsPreferences.register(in: defaults)
        XCTAssertEqual(defaults.integer(forKey: "hotKeyCode"), 3)
        XCTAssertEqual(defaults.integer(forKey: "hotKeyModifiers"), Int(controlKey | optionKey | shiftKey))
        XCTAssertTrue(SettingsPreferences.searchOptions(in: defaults).pinyin)
        XCTAssertEqual(SettingsPreferences.searchOptions(in: defaults).sort, .relevance)
        XCTAssertFalse(defaults.bool(forKey: "sortAscending"))
        XCTAssertFalse(defaults.bool(forKey: "skippedFullDiskAccess"))
        defaults.set(false, forKey: "pinyinEnabled")
        defaults.set("name", forKey: "sortKey"); defaults.set(true, forKey: "sortAscending")
        defaults.set(["/a", "/a/sub"], forKey: "userExcludedPaths")
        XCTAssertFalse(SettingsPreferences.searchOptions(in: defaults).pinyin)
        XCTAssertEqual(SettingsPreferences.searchOptions(in: defaults).sort, .name)
        XCTAssertTrue(SettingsPreferences.searchOptions(in: defaults).ascending)
        XCTAssertEqual(SettingsPreferences.indexConfig(limited: true, in: defaults).userExcludedPaths, ["/a"])
        XCTAssertTrue(SettingsPreferences.indexConfig(limited: true, in: defaults).limitedMode)
    }

    func testT52UpdateConfig() throws {
        try skipIfCI("Live index rebuild timing is covered locally.")
        let raw = FileManager.default.temporaryDirectory.appendingPathComponent("OilFindM4-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: raw, withIntermediateDirectories: true)
        let pointer = try XCTUnwrap(realpath(raw.path, nil)); defer { free(pointer) }
        let base = URL(fileURLWithPath: String(cString: pointer))
        defer { try? FileManager.default.removeItem(at: base) }
        let root = base.appendingPathComponent("tree"), db = base.appendingPathComponent("index.oilfind")
        for directory in ["a", "b", "c", "bulk"] {
            try FileManager.default.createDirectory(at: root.appendingPathComponent(directory), withIntermediateDirectories: true)
            try Data("sample".utf8).write(to: root.appendingPathComponent(directory + "/leaf.txt"))
        }
        let original = IndexConfig(rootPath: root.path, indexSystemDirs: true)
        let manager = IndexManager(config: original, dbURL: db, settingsDefaults: nil)
        let scanLock = NSLock(), scanEntered = DispatchSemaphore(value: 0), resumeScan = DispatchSemaphore(value: 0)
        var pauseNextScan = false
        manager.onScannerCreated = { _ in
            scanLock.lock(); let pause = pauseNextScan; pauseNextScan = false; scanLock.unlock()
            if pause { scanEntered.signal(); _ = resumeScan.wait(timeout: .now() + 8) }
        }
        // Control the in-flight boundary directly instead of relying on thousands of files
        // to keep a rebuild observable under varying filesystem and machine load.
        defer { resumeScan.signal() }
        manager.start(); defer { manager.stop() }
        XCTAssertTrue(eventually { manager.state == .ready && !manager.isRescanning })
        let first = try XCTUnwrap(manager.store)
        manager.updateConfig(original)
        RunLoop.current.run(until: Date().addingTimeInterval(0.2))
        XCTAssertTrue(manager.store === first); XCTAssertFalse(manager.isRescanning)

        var configA = original; configA.userExcludedPaths = [root.path + "/a"]
        scanLock.lock(); pauseNextScan = true; scanLock.unlock()
        manager.updateConfig(configA)
        XCTAssertTrue(eventually { scanEntered.wait(timeout: .now()) == .success })
        XCTAssertTrue(manager.isRescanning)
        XCTAssertNil(manager.store, "The old catalog is not exposed during an exclusion rebuild")
        XCTAssertNotNil(Searcher.search(Query.parse("leaf"), in: first))
        resumeScan.signal()
        XCTAssertTrue(eventually { !manager.isRescanning && manager.store?.configFingerprint == configA.fingerprint })
        let second = try XCTUnwrap(manager.store)
        XCTAssertNil(second.read { second.resolve(path: root.path + "/a") })
        manager.saveNow()
        XCTAssertEqual(IndexStore.load(from: db.path)?.configFingerprint, configA.fingerprint)

        scanLock.lock(); pauseNextScan = true; scanLock.unlock()
        manager.rescan()
        XCTAssertTrue(eventually { scanEntered.wait(timeout: .now()) == .success })
        XCTAssertTrue(manager.isRescanning)
        var configB = original; configB.userExcludedPaths = [root.path + "/b"]
        var configC = original; configC.userExcludedPaths = [root.path + "/c"]
        manager.updateConfig(configB); manager.updateConfig(configC)
        _ = manager.watcherEpochForTesting
        resumeScan.signal()
        XCTAssertTrue(eventually { !manager.isRescanning && manager.store?.configFingerprint == configC.fingerprint })
        let final = try XCTUnwrap(manager.store)
        final.read {
            XCTAssertNotNil(final.resolve(path: root.path + "/a/leaf.txt"))
            XCTAssertNotNil(final.resolve(path: root.path + "/b/leaf.txt"))
            XCTAssertNil(final.resolve(path: root.path + "/c"))
        }
        manager.saveNow()
        XCTAssertEqual(IndexStore.load(from: db.path)?.configFingerprint, configC.fingerprint)
        // The replacement updater accepts a formerly excluded path and rejects the latest exclusion.
        try Data().write(to: root.appendingPathComponent("c/hidden.txt"))
        try Data().write(to: root.appendingPathComponent("a/visible.txt"))
        XCTAssertTrue(eventually { manager.store.map { s in s.read { s.resolve(path: root.path + "/a/visible.txt") != nil } } ?? false })
        let live = try XCTUnwrap(manager.store)
        XCTAssertNil(live.read { live.resolve(path: root.path + "/c/hidden.txt") })
        var excludeRoot = original; excludeRoot.userExcludedPaths = ["/"]
        manager.updateConfig(excludeRoot)
        XCTAssertTrue(eventually { !manager.isRescanning && manager.store?.configFingerprint == excludeRoot.fingerprint })
        let onlyRoot = try XCTUnwrap(manager.store)
        XCTAssertEqual(onlyRoot.read { onlyRoot.liveCount }, 1)
    }
    func testReorderedExclusionsKeepCatalogWithoutRebuilding() throws {
        let raw = FileManager.default.temporaryDirectory.appendingPathComponent("OilFindReordered-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: raw, withIntermediateDirectories: true)
        let pointer = try XCTUnwrap(realpath(raw.path, nil)); defer { free(pointer) }
        let base = URL(fileURLWithPath: String(cString: pointer))
        defer { try? FileManager.default.removeItem(at: base) }
        let root = base.appendingPathComponent("tree"), db = base.appendingPathComponent("index.oilfind")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        for name in ["a", "b"] {
            try FileManager.default.createDirectory(at: root.appendingPathComponent(name), withIntermediateDirectories: true)
            try Data().write(to: root.appendingPathComponent(name + "/hidden.txt"))
        }
        try Data().write(to: root.appendingPathComponent("visible.txt"))
        let original = IndexConfig(rootPath: root.path, userExcludedPaths: [root.path + "/a", root.path + "/b"], indexSystemDirs: true)
        let manager = IndexManager(config: original, dbURL: db, settingsDefaults: nil)
        let rescanLock = NSLock()
        var rescans = 0
        manager.onRescanStart = { rescanLock.lock(); rescans += 1; rescanLock.unlock() }
        manager.start(); defer { manager.stop() }
        XCTAssertTrue(eventually { manager.state == .ready && !manager.isRescanning })
        let published = try XCTUnwrap(manager.store)
        var reordered = original; reordered.userExcludedPaths.reverse()
        XCTAssertEqual(original.fingerprint, reordered.fingerprint)
        manager.updateConfig(reordered)
        XCTAssertTrue(manager.store === published, "Reordering must not withdraw the catalog")
        _ = manager.watcherEpochForTesting
        XCTAssertTrue(manager.store === published, "Equivalent configurations must keep the catalog available")
        XCTAssertFalse(manager.isRescanning)
        rescanLock.lock(); let observedRescans = rescans; rescanLock.unlock()
        XCTAssertEqual(observedRescans, 0)
        if let store = manager.store {
            let result = try XCTUnwrap(Searcher.search(Query.parse("visible"), in: store))
            XCTAssertEqual(result.total, 1)
        }
    }

    func testExcludedPathsHideOldCatalogThroughFailureAndSupersededScans() throws {
        let raw = FileManager.default.temporaryDirectory.appendingPathComponent("OilFindExclusions-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: raw, withIntermediateDirectories: true)
        let pointer = try XCTUnwrap(realpath(raw.path, nil)); defer { free(pointer) }
        let base = URL(fileURLWithPath: String(cString: pointer))
        defer { try? FileManager.default.removeItem(at: base) }
        let root = base.appendingPathComponent("tree"), db = base.appendingPathComponent("index.oilfind")
        for directory in ["private", "second", "public"] {
            try FileManager.default.createDirectory(at: root.appendingPathComponent(directory), withIntermediateDirectories: true)
            try Data().write(to: root.appendingPathComponent(directory + "/leaf.txt"))
        }
        let original = IndexConfig(rootPath: root.path, indexSystemDirs: true)
        let manager = IndexManager(config: original, dbURL: db, settingsDefaults: nil)
        let cancelled = DispatchSemaphore(value: 0), firstEntered = DispatchSemaphore(value: 0), firstRelease = DispatchSemaphore(value: 0)
        let secondEntered = DispatchSemaphore(value: 0), secondRelease = DispatchSemaphore(value: 0), scanLock = NSLock()
        var scans = 0
        manager.onScannerCreated = { scanner in
            scanLock.lock(); scans += 1; let scan = scans; scanLock.unlock()
            if scan == 2 { scanner.cancel(); cancelled.signal() }
            if scan == 3 { firstEntered.signal(); _ = firstRelease.wait(timeout: .now() + 8) }
            if scan == 4 { secondEntered.signal(); _ = secondRelease.wait(timeout: .now() + 8) }
        }
        defer { firstRelease.signal(); secondRelease.signal(); manager.stop() }
        manager.start()
        XCTAssertTrue(eventually { manager.state == .ready && !manager.isRescanning })
        let old = try XCTUnwrap(manager.store)
        XCTAssertNotNil(old.read { old.resolve(path: root.path + "/private/leaf.txt") })

        var first = original; first.userExcludedPaths = [root.path + "/private"]
        manager.updateConfig(first)
        XCTAssertNil(manager.store, "Old results must be withdrawn synchronously")
        XCTAssertTrue(eventually { cancelled.wait(timeout: .now()) == .success })
        XCTAssertTrue(eventually { !manager.isRescanning })
        XCTAssertNil(manager.store, "A failed rebuild must not expose the old catalog")

        manager.rescan()
        XCTAssertTrue(eventually { firstEntered.wait(timeout: .now()) == .success })
        var latest = first; latest.userExcludedPaths.append(root.path + "/second")
        manager.updateConfig(latest)
        XCTAssertNil(manager.store)
        firstRelease.signal()
        XCTAssertTrue(eventually { secondEntered.wait(timeout: .now()) == .success })
        XCTAssertNil(manager.store, "A superseded scan must remain hidden")
        secondRelease.signal()
        XCTAssertTrue(eventually { !manager.isRescanning && manager.store?.configFingerprint == latest.fingerprint })
        let safe = try XCTUnwrap(manager.store)
        safe.read {
            XCTAssertNil(safe.resolve(path: root.path + "/private/leaf.txt"))
            XCTAssertNil(safe.resolve(path: root.path + "/second/leaf.txt"))
            XCTAssertNotNil(safe.resolve(path: root.path + "/public/leaf.txt"))
        }
    }

    private func eventually(_ predicate: () -> Bool) -> Bool {
        let deadline = Date().addingTimeInterval(8)
        repeat {
            if predicate() { return true }
            RunLoop.current.run(until: Date().addingTimeInterval(0.002))
        } while Date() < deadline
        return predicate()
    }
}
