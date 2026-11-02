import XCTest
@testable import OilFindCore
import DiskArbitration
import Combine
@testable import OilFindApp

final class ExternalDriveTests: XCTestCase {
    private let uuid = "0A000000-0000-0000-0000-000000000001"
    private let secondUUID = "0A000000-0000-0000-0000-000000000002"
    private var volume: ExternalVolume { .init(uuid: uuid, name: "T7 Shield", path: "/Volumes/T7 Shield", device: "disk7") }
    private func temporaryDirectory() -> URL {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("external-drive-tests-" + UUID().uuidString)
        addTeardownBlock { try? FileManager.default.removeItem(at: url) }
        return url
    }
    private func store(path: String) -> IndexStore {
        var buffer = ScanBuffer()
        buffer.ids = [0, 1]; buffer.parents = [0, 0]; buffer.sizes = [0, 1024]; buffer.mtimes = [0, 1_790_000_000]
        buffer.flags = [1, 0x80]; buffer.depths = [2, 3]; buffer.kinds = [1, 3]
        buffer.nameLens = [0, 9]; buffer.nameBytes = Array("README.md".utf8)
        return IndexStore(scan: ScanOutput(buffers: [buffer], count: 2, homeIndex: .max, elapsed: 0, finishedAt: 0), config: IndexConfig(rootPath: path))
    }
    final class Monitor: ExternalDriveMonitoring {
        var onChange: (() -> Void)?
        var beforeEject: ((String) -> Void)?
        var beforeUnmount: ((String) -> Void)?
        var starts = 0, stops = 0
        func start() { starts += 1 }
        func stop() { stops += 1 }
    }
    final class Index: ExternalDriveIndex {
        var store: IndexStore?
        var state: IndexManager.State = .loading
        var scannedCount = 0
        var onStateChange: ((IndexManager.State) -> Void)?
        var onIndexChange: (() -> Void)?
        var starts = 0, stops = 0
        var configs: [IndexConfig] = []
        func updateConfig(_ config: IndexConfig) { configs.append(config) }
        func start() { starts += 1 }
        var onStop: (() -> Void)?
        func stop() { stops += 1; onStop?() }
        func complete(_ store: IndexStore) { self.store = store; state = .ready; onIndexChange?(); onStateChange?(.ready) }
    }
    private func waitUntil(_ description: String, timeout: TimeInterval = 10,
                           file: StaticString = #filePath, line: UInt = #line,
                           _ condition: () -> Bool) throws {
        let deadline = ProcessInfo.processInfo.systemUptime + timeout
        repeat {
            if condition() { return }
            RunLoop.current.run(until: Date().addingTimeInterval(0.01))
        } while ProcessInfo.processInfo.systemUptime < deadline
        _ = try XCTUnwrap(condition() ? true : nil, "Timed out waiting for " + description, file: file, line: line)
    }
    private func assertRemains(_ description: String, duration: TimeInterval,
                               file: StaticString = #filePath, line: UInt = #line,
                               _ condition: () -> Bool) throws {
        let deadline = ProcessInfo.processInfo.systemUptime + duration
        repeat {
            _ = try XCTUnwrap(condition() ? true : nil, description, file: file, line: line)
            RunLoop.current.run(until: Date().addingTimeInterval(0.01))
        } while ProcessInfo.processInfo.systemUptime < deadline
        _ = try XCTUnwrap(condition() ? true : nil, description, file: file, line: line)
    }

    func testFirstRunWaitsForExplicitExternalIndexingOptIn() throws {
        let suite = "OilFindExternalOptIn-" + UUID().uuidString
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let directory = temporaryDirectory(), monitor = Monitor()
        let persistence = ExternalDrivePersistence(directory: directory)
        let lock = NSLock()
        var discoveries = 0, configs: [IndexConfig] = [], indexes: [Index] = []
        let catalog = ExternalDriveCatalog(directory: directory, monitor: monitor, discover: {
            lock.lock(); discoveries += 1; lock.unlock()
            return [self.volume]
        }, settingsDefaults: defaults, makeIndex: { config, _, _ in
            configs.append(config); let index = Index(); indexes.append(index); return index
        })
        defer { catalog.stop() }
        XCTAssertFalse(ExternalDrivePreferences().enabled)
        XCTAssertFalse(catalog.enabled)
        XCTAssertFalse(persistence.load().enabled)
        catalog.start()
        try waitUntil("initial external preferences") { catalog.ready }
        XCTAssertFalse(catalog.enabled)
        XCTAssertEqual(monitor.starts, 0)
        lock.lock(); let before = discoveries; lock.unlock()
        XCTAssertEqual(before, 0)
        XCTAssertTrue(indexes.isEmpty)
        guard !catalog.enabled else { return }

        let excluded = volume.path + "/Private"
        defaults.set([excluded], forKey: "userExcludedPaths")
        catalog.setEnabled(true)
        try waitUntil("explicit external opt-in") { indexes.count == 1 && indexes[0].starts == 1 }
        XCTAssertEqual(monitor.starts, 1)
        let config = try XCTUnwrap(configs.first)
        Array((excluded + "/leaf").utf8).withUnsafeBufferPointer { XCTAssertTrue(config.isExcluded(path: $0)) }
        try waitUntil("enabled state persisted") { persistence.load().enabled }
        catalog.setEnabled(false)
        try waitUntil("disabled state persisted") { !persistence.load().enabled }
        XCTAssertEqual(indexes[0].stops, 1)

        let restoredMonitor = Monitor()
        let restored = ExternalDriveCatalog(directory: directory, monitor: restoredMonitor,
                                            discover: { XCTFail("Disabled catalog must not discover volumes"); return [] },
                                            settingsDefaults: defaults, makeIndex: { _, _, _ in XCTFail("Disabled catalog must not index"); return Index() })
        defer { restored.stop() }
        restored.start()
        try waitUntil("disabled state restored") { restored.ready }
        XCTAssertFalse(restored.enabled)
        XCTAssertEqual(restoredMonitor.starts, 0)
    }

    func testPreviouslyEnabledIsolatedExternalSettingsRemainEnabled() throws {
        let suite = "OilFindExternalSaved-" + UUID().uuidString
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let directory = temporaryDirectory(), monitor = Monitor()
        let persistence = ExternalDrivePersistence(directory: directory)
        persistence.save(.init(enabled: true))
        let index = Index()
        let catalog = ExternalDriveCatalog(directory: directory, monitor: monitor, discover: { [self.volume] },
                                          settingsDefaults: defaults, makeIndex: { _, _, _ in index })
        defer { catalog.stop() }
        catalog.start()
        try waitUntil("previously enabled catalog") { catalog.ready && catalog.enabled && index.starts == 1 }
        XCTAssertEqual(monitor.starts, 1)
        XCTAssertTrue(persistence.load().enabled)
    }

    func testUserExclusionsAndScopeFollowExternalDriveSettings() throws {
        let suite = "OilFindExternalExclusions-" + UUID().uuidString
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let initialPath = volume.path + "/Private"
        defaults.set([initialPath], forKey: "userExcludedPaths")
        defaults.set(true, forKey: "indexPackageContents")
        let directory = temporaryDirectory()
        var initialConfig: IndexConfig?
        let index = Index()
        let catalog = ExternalDriveCatalog(directory: directory, monitor: Monitor(), initialPreferences: .init(enabled: true),
                                          discover: { XCTFail("Unexpected volume discovery"); return [] },
                                          settingsDefaults: defaults, makeIndex: { config, _, _ in
            initialConfig = config; return index
        })
        defer { catalog.stop() }
        catalog.updateMounted([volume])
        let initial = try XCTUnwrap(initialConfig)
        XCTAssertEqual(initial.rootPath, volume.path)
        XCTAssertEqual(initial.userExcludedPaths, [initialPath])
        XCTAssertTrue(initial.indexPackageContents)
        Array((initialPath + "/leaf").utf8).withUnsafeBufferPointer { XCTAssertTrue(initial.isExcluded(path: $0)) }
        Array((volume.path + "/Public/leaf").utf8).withUnsafeBufferPointer { XCTAssertFalse(initial.isExcluded(path: $0)) }

        let changedPath = volume.path + "/Other"
        defaults.set([changedPath], forKey: "userExcludedPaths")
        defaults.set(false, forKey: "indexPackageContents")
        catalog.updateSettings()
        let changed = try XCTUnwrap(index.configs.last)
        XCTAssertEqual(changed.rootPath, volume.path)
        XCTAssertEqual(changed.userExcludedPaths, [changedPath])
        XCTAssertFalse(changed.indexPackageContents)
        Array((changedPath + "/leaf").utf8).withUnsafeBufferPointer { XCTAssertTrue(changed.isExcluded(path: $0)) }
        Array((initialPath + "/leaf").utf8).withUnsafeBufferPointer { XCTAssertFalse(changed.isExcluded(path: $0)) }
        try waitUntil("isolated drive preferences written") {
            FileManager.default.fileExists(atPath: directory.appendingPathComponent("drives.json").path)
        }
    }

    func testExclusionFollowsRecordedUUIDAcrossRemountsAndRestart() throws {
        let suite = "OilFindExternalRemount-" + UUID().uuidString
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let original = volume.path + "/Private"
        defaults.set([original], forKey: "userExcludedPaths")
        let directory = temporaryDirectory()
        var configs: [IndexConfig] = [], indexes: [Index] = []
        let catalog = ExternalDriveCatalog(directory: directory, monitor: Monitor(), initialPreferences: .init(enabled: true),
                                          discover: { XCTFail("Unexpected volume discovery"); return [] },
                                          settingsDefaults: defaults, makeIndex: { config, _, _ in
            configs.append(config); let index = Index(); indexes.append(index); return index
        })
        defer { catalog.stop() }
        catalog.updateMounted([volume])
        try XCTUnwrap(indexes.last).complete(store(path: volume.path))
        var renamed = volume; renamed.name = "Renamed"; renamed.path = "/Volumes/Renamed"
        catalog.updateMounted([renamed])
        XCTAssertEqual(indexes.first?.stops, 1)
        XCTAssertTrue(catalog.searchSources().isEmpty)
        let mapped = try XCTUnwrap(configs.last)
        XCTAssertEqual(Set(mapped.userExcludedPaths), Set([original, renamed.path + "/Private"]))
        Array((renamed.path + "/Private/leaf").utf8).withUnsafeBufferPointer { XCTAssertTrue(mapped.isExcluded(path: $0)) }
        Array((renamed.path + "/Private-other/leaf").utf8).withUnsafeBufferPointer { XCTAssertFalse(mapped.isExcluded(path: $0)) }
        try XCTUnwrap(indexes.last).complete(store(path: renamed.path))
        let extensionSource = SearchSource(id: "extension", displayName: "", isOnline: true, store: store(path: renamed.path))
        XCTAssertTrue(catalog.filteredExtensionSources([extensionSource]).isEmpty)

        var third = renamed; third.path = "/Volumes/Third"
        catalog.updateMounted([third])
        let historical = SearchSource(id: "historical", displayName: "", isOnline: false, store: store(path: renamed.path))
        XCTAssertTrue(catalog.filteredExtensionSources([historical]).isEmpty)
        XCTAssertEqual(defaults.stringArray(forKey: "userExcludedPaths"), [original])
        let persistence = ExternalDrivePersistence(directory: directory)
        try waitUntil("remount history persisted") {
            let record = persistence.load().drives.first
            return record?.path == third.path && Set(record?.previousPaths ?? []) == Set([self.volume.path, renamed.path])
        }
        let persisted = persistence.load(), restoredDirectory = temporaryDirectory()
        var restoredConfig: IndexConfig?
        let restored = ExternalDriveCatalog(directory: restoredDirectory, monitor: Monitor(), initialPreferences: persisted,
                                           discover: { XCTFail("Unexpected volume discovery"); return [] },
                                           settingsDefaults: defaults, makeIndex: { config, _, _ in
            restoredConfig = config; return Index()
        })
        defer { restored.stop() }
        var fourth = third; fourth.path = "/Volumes/Fourth"
        restored.updateMounted([fourth])
        let afterRestart = try XCTUnwrap(restoredConfig)
        Array((fourth.path + "/Private/leaf").utf8).withUnsafeBufferPointer { XCTAssertTrue(afterRestart.isExcluded(path: $0)) }
        XCTAssertEqual(defaults.stringArray(forKey: "userExcludedPaths"), [original])
        try waitUntil("restored remount history persisted") {
            ExternalDrivePersistence(directory: restoredDirectory).load().drives.first?.path == fourth.path
        }
    }

    func testUnmatchedExclusionsPauseIndexesUntilUserReselects() throws {
        let suite = "OilFindExternalReview-" + UUID().uuidString
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let unknown = "/Volumes/Unknown/Private", current = volume.path + "/Private"
        defaults.set([unknown], forKey: "userExcludedPaths")
        var configs: [IndexConfig] = [], indexes: [Index] = []
        let directory = temporaryDirectory()
        let catalog = ExternalDriveCatalog(directory: directory, monitor: Monitor(), initialPreferences: .init(enabled: true),
                                          discover: { XCTFail("Unexpected volume discovery"); return [] },
                                          settingsDefaults: defaults, makeIndex: { config, _, _ in
            configs.append(config); let index = Index(); indexes.append(index); return index
        })
        defer { catalog.stop() }
        catalog.updateMounted([volume])
        XCTAssertTrue(catalog.exclusionReviewRequired)
        XCTAssertEqual(catalog.rows.first?.status, .exclusionReview)
        XCTAssertEqual(catalog.knownDrives.first?.enabled, false)
        XCTAssertTrue(indexes.isEmpty)
        XCTAssertEqual(defaults.stringArray(forKey: "userExcludedPaths"), [unknown])
        XCTAssertTrue(L10n.text("driveExclusionReview", chinese: false).contains("First add"))

        defaults.set([unknown, current], forKey: "userExcludedPaths")
        catalog.updateSettings()
        XCTAssertTrue(catalog.exclusionReviewRequired)
        XCTAssertTrue(indexes.isEmpty)
        defaults.set([current], forKey: "userExcludedPaths")
        catalog.updateSettings()
        XCTAssertFalse(catalog.exclusionReviewRequired)
        XCTAssertEqual(indexes.count, 1)
        let config = try XCTUnwrap(configs.last)
        Array((current + "/leaf").utf8).withUnsafeBufferPointer { XCTAssertTrue(config.isExcluded(path: $0)) }
        try XCTUnwrap(indexes.last).complete(store(path: volume.path))
        XCTAssertEqual(catalog.searchSources().count, 1)
        defaults.set([unknown], forKey: "userExcludedPaths")
        XCTAssertTrue(catalog.searchSources().isEmpty, "An unresolved rule must hide old sources immediately")
        catalog.updateSettings()
        XCTAssertTrue(catalog.exclusionReviewRequired)
        XCTAssertEqual(indexes.first?.stops, 1)
        XCTAssertTrue(catalog.searchSources().isEmpty)
        let other = SearchSource(id: "extension", displayName: "", isOnline: true, store: store(path: "/Volumes/Other"))
        XCTAssertTrue(catalog.filteredExtensionSources([other]).isEmpty)
        try waitUntil("isolated review metadata persisted") {
            FileManager.default.fileExists(atPath: directory.appendingPathComponent("drives.json").path)
        }
    }

    func testAmbiguousOldMountPausesAllPossibleDrives() throws {
        let suite = "OilFindExternalAmbiguous-" + UUID().uuidString
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let old = "/Volumes/Old", exclusion = old + "/Private"
        defaults.set([exclusion], forKey: "userExcludedPaths")
        var first = volume; first.path = "/Volumes/First"
        var second = volume; second.uuid = secondUUID; second.path = "/Volumes/Second"; second.device = "disk8"
        var firstRecord = ExternalDriveRecord(id: uuid, name: first.name, path: first.path, lastConnected: Date())
        var secondRecord = ExternalDriveRecord(id: secondUUID, name: second.name, path: second.path, lastConnected: Date())
        firstRecord.previousPaths = [old]; secondRecord.previousPaths = [old, first.path]
        var created = 0, configs: [String: IndexConfig] = [:]
        let directory = temporaryDirectory()
        let catalog = ExternalDriveCatalog(directory: directory, monitor: Monitor(),
                                          initialPreferences: .init(enabled: true, drives: [firstRecord, secondRecord]),
                                          discover: { XCTFail("Unexpected volume discovery"); return [] },
                                          settingsDefaults: defaults, makeIndex: { config, _, id in
            created += 1; configs[id] = config; return Index()
        })
        defer { catalog.stop() }
        catalog.updateMounted([first, second])
        XCTAssertTrue(catalog.exclusionReviewRequired)
        XCTAssertEqual(created, 0)
        XCTAssertTrue(catalog.rows.allSatisfy { $0.status == .exclusionReview })
        XCTAssertTrue(catalog.knownDrives.allSatisfy { !$0.enabled })
        XCTAssertEqual(defaults.stringArray(forKey: "userExcludedPaths"), [exclusion])

        // A deliberate re-selection binds the new rule to the mounted UUID even when
        // the same mount name also appears in another drive's history.
        let selected = first.path + "/Private"
        XCTAssertTrue(catalog.bindSelectedExclusions([selected]))
        defaults.set([exclusion, selected], forKey: "userExcludedPaths")
        catalog.updateSettings()
        XCTAssertTrue(catalog.exclusionReviewRequired)
        defaults.set([selected], forKey: "userExcludedPaths")
        catalog.updateSettings()
        XCTAssertFalse(catalog.exclusionReviewRequired)
        XCTAssertEqual(created, 2)
        var renamed = first; renamed.path = "/Volumes/First-Renamed"
        catalog.updateMounted([renamed, second])
        let config = try XCTUnwrap(configs[uuid])
        Array((renamed.path + "/Private/leaf").utf8).withUnsafeBufferPointer { XCTAssertTrue(config.isExcluded(path: $0)) }
        XCTAssertEqual(defaults.stringArray(forKey: "userExcludedPaths"), [selected])
        let persistence = ExternalDrivePersistence(directory: directory)
        try waitUntil("explicit exclusion binding persisted") {
            let saved = persistence.load()
            return saved.drives.first(where: { $0.id == self.uuid })?.path == renamed.path
                && saved.exclusionBindings?[selected] == ExternalExclusionBinding(id: self.uuid, root: first.path)
        }
        let persisted = persistence.load(), restoredDirectory = temporaryDirectory()
        var restoredConfigs: [String: IndexConfig] = [:]
        let restored = ExternalDriveCatalog(directory: restoredDirectory, monitor: Monitor(), initialPreferences: persisted,
                                           discover: { XCTFail("Unexpected volume discovery"); return [] },
                                           settingsDefaults: defaults, makeIndex: { config, _, id in
            restoredConfigs[id] = config; return Index()
        })
        defer { restored.stop() }
        var next = renamed; next.path = "/Volumes/First-Again"
        restored.updateMounted([next, second])
        XCTAssertFalse(restored.exclusionReviewRequired)
        let afterRestart = try XCTUnwrap(restoredConfigs[uuid])
        Array((next.path + "/Private/leaf").utf8).withUnsafeBufferPointer { XCTAssertTrue(afterRestart.isExcluded(path: $0)) }
        try waitUntil("restored explicit binding persisted") {
            ExternalDrivePersistence(directory: restoredDirectory).load().drives.first(where: { $0.id == self.uuid })?.path == next.path
        }
    }

    func testReselectingTheSameAmbiguousPathBindsCurrentUUIDWithoutChangingSettings() throws {
        let suite = "OilFindExternalReselection-" + UUID().uuidString
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let original = "/Volumes/Old/Private"
        defaults.set([original], forKey: "userExcludedPaths")
        var first = volume; first.path = "/Volumes/Old"
        var second = volume; second.uuid = secondUUID; second.path = "/Volumes/Second"; second.device = "disk8"
        let firstRecord = ExternalDriveRecord(id: uuid, name: first.name, path: first.path, lastConnected: Date())
        var secondRecord = ExternalDriveRecord(id: secondUUID, name: second.name, path: second.path, lastConnected: Date())
        secondRecord.previousPaths = [first.path]
        let directory = temporaryDirectory()
        var configs: [String: IndexConfig] = [:]
        let catalog = ExternalDriveCatalog(directory: directory, monitor: Monitor(),
                                          initialPreferences: .init(enabled: true, drives: [firstRecord, secondRecord]),
                                          discover: { XCTFail("Unexpected volume discovery"); return [] },
                                          settingsDefaults: defaults, makeIndex: { config, _, id in
            configs[id] = config; return Index()
        })
        defer { catalog.stop() }
        catalog.updateMounted([first, second])
        XCTAssertTrue(catalog.exclusionReviewRequired)
        XCTAssertTrue(catalog.bindSelectedExclusions([original]))
        catalog.updateSettings()
        XCTAssertFalse(catalog.exclusionReviewRequired)
        XCTAssertEqual(defaults.stringArray(forKey: "userExcludedPaths"), [original])
        let selected = try XCTUnwrap(configs[uuid])
        Array((original + "/leaf").utf8).withUnsafeBufferPointer { XCTAssertTrue(selected.isExcluded(path: $0)) }
        try waitUntil("same-path re-selection persisted") {
            ExternalDrivePersistence(directory: directory).load().exclusionBindings?[original]
                == ExternalExclusionBinding(id: self.uuid, root: first.path)
        }
    }

    func testVolumeEligibilityUsesLocalUserVisibleIdentityAndExcludesSpecialVolumes() {
        XCTAssertTrue(volume.eligible())
        var candidate = volume; candidate.local = false; XCTAssertFalse(candidate.eligible())
        candidate = volume; candidate.startup = true; XCTAssertFalse(candidate.eligible())
        candidate = volume; candidate.visible = false; XCTAssertFalse(candidate.eligible())
        candidate = volume; candidate.diskImage = true
        XCTAssertFalse(candidate.eligible()); XCTAssertTrue(candidate.eligible(allowDiskImages: true))
        candidate = volume; candidate.timeMachine = true; XCTAssertFalse(candidate.eligible())
        candidate = volume; candidate.writable = false; XCTAssertTrue(candidate.eligible())
        candidate.optical = true; XCTAssertFalse(candidate.eligible())
        candidate = volume; candidate.uuid = "../../outside"; XCTAssertFalse(candidate.eligible())
        for role in ["Preboot", "Recovery", "VM", "Update", "Backup"] {
            candidate = volume; candidate.roles = [role]; XCTAssertFalse(candidate.eligible())
        }
        candidate = volume; candidate.path = "/System/Volumes/Hidden"; XCTAssertFalse(candidate.eligible())
        candidate = volume; candidate.path = "/"; XCTAssertFalse(candidate.eligible())
    }

    func testDiskArbitrationCFUUIDIsDecodedForPartitionUnmount() throws {
        let value = try XCTUnwrap(CFUUIDCreateFromString(kCFAllocatorDefault, uuid as CFString))
        let description: [String: Any] = [kDADiskDescriptionVolumeUUIDKey as String: value]
        XCTAssertEqual(ExternalVolumeMonitor.volumeUUID(in: description), uuid)
        XCTAssertNil(ExternalVolumeMonitor.volumeUUID(in: [kDADiskDescriptionVolumeUUIDKey as String: "not-a-uuid"]))
    }

    func testUUIDCatalogPersistenceSurvivesRenameAndRejectsInvalidRecords() throws {
        let persistence = ExternalDrivePersistence(directory: temporaryDirectory()), date = Date(timeIntervalSince1970: 42)
        let record = ExternalDriveRecord(id: uuid.lowercased(), name: "Renamed", path: "/Volumes/T7 1", lastConnected: date, enabled: false)
        persistence.save(.init(enabled: false, drives: [record, record, .init(id: "../../outside", name: "Bad", path: "/tmp", lastConnected: date)]))
        let loaded = persistence.load()
        XCTAssertFalse(loaded.enabled); XCTAssertEqual(loaded.drives.count, 1)
        XCTAssertEqual(loaded.drives.first?.id, uuid); XCTAssertEqual(loaded.drives.first?.name, "Renamed")
        XCTAssertEqual(loaded.drives.first?.path, "/Volumes/T7 1"); XCTAssertFalse(try XCTUnwrap(loaded.drives.first).enabled)
        XCTAssertEqual(persistence.indexURL(uuid), persistence.indexURL(uuid.lowercased()))
        XCTAssertTrue(FileManager.default.fileExists(atPath: persistence.metadataURL.path))
    }

    func testPreEjectStopsWholeDeviceBeforeReturningAndWithdrawsSources() throws {
        var indexes: [Index] = []
        let catalog = ExternalDriveCatalog(directory: temporaryDirectory(), monitor: Monitor(),
                                          initialPreferences: .init(enabled: true), makeIndex: { _, _, _ in
            let index = Index(); indexes.append(index); return index
        })
        defer { catalog.stop() }
        var second = volume; second.uuid = secondUUID; second.name = "Second"; second.path = "/Volumes/Second"
        catalog.updateMounted([volume, second])
        for index in indexes { index.complete(store(path: volume.path)) }
        catalog.prepareToEject(device: "disk7")
        XCTAssertTrue(indexes.allSatisfy { $0.stops == 1 }, "Approvals must synchronously release every partition on the device")
        try waitUntil("ejected drive rows") { catalog.rows.count == 2 && catalog.rows.allSatisfy { $0.status == .offline } }
        XCTAssertTrue(catalog.searchSources().isEmpty)
        catalog.updateMounted([])
        XCTAssertEqual(indexes.count, 2, "Disconnecting never creates an offline index")
    }

    func testPartitionUnmountLeavesSiblingOnlineAndMasterSwitchRestoresCancelledEject() throws {
        var second = volume; second.uuid = secondUUID; second.name = "Second"; second.path = "/Volumes/Second"
        var mounted = [volume, second], indexes: [Index] = [], indexesByID: [String: Index] = [:]
        let catalog = ExternalDriveCatalog(directory: temporaryDirectory(), monitor: Monitor(),
                                          initialPreferences: .init(enabled: true), discover: { mounted }, makeIndex: { _, _, id in
            let index = Index(); indexes.append(index); indexesByID[id] = index; return index
        })
        defer { catalog.stop() }
        catalog.updateMounted(mounted)
        for (uuid, path) in [(uuid, volume.path), (secondUUID, second.path)] {
            try XCTUnwrap(indexesByID[uuid]).complete(store(path: path))
        }
        catalog.prepareToUnmount(uuid: uuid)
        XCTAssertEqual(indexes.map(\.stops).reduce(0, +), 1)
        try waitUntil("unmounted partition row") { catalog.rows.first { $0.id == self.uuid }?.status == .offline }
        let sibling = catalog.searchSources().first { $0.id == secondUUID }
        XCTAssertEqual(sibling?.isOnline, true)
        mounted = [second]; catalog.updateMounted(mounted)
        XCTAssertEqual(indexes.map(\.stops).reduce(0, +), 1)
        catalog.prepareToEject(device: "disk7")
        try waitUntil("ejected sibling row") { catalog.rows.first { $0.id == self.secondUUID }?.status == .offline }
        XCTAssertEqual(indexes.map(\.stops).reduce(0, +), 2)
        catalog.setEnabled(false); catalog.setEnabled(true)
        try waitUntil("still-mounted sibling to restart") { indexes.map(\.starts).reduce(0, +) == 3 }
        XCTAssertEqual(indexes.map(\.starts).reduce(0, +), 3, "A deliberate master switch resumes the still-mounted disk")
    }

    func testSlowUnrelatedAndDuplicateApprovalsCannotAutomaticallyRestartIndexing() throws {
        var indexes: [Index] = []
        let mounted = [volume]
        let catalog = ExternalDriveCatalog(directory: temporaryDirectory(), monitor: Monitor(),
                                          initialPreferences: .init(enabled: true), discover: { mounted }, makeIndex: { _, _, _ in
            let index = Index(); indexes.append(index); return index
        })
        defer { catalog.stop() }
        catalog.updateMounted(mounted)
        let first = try XCTUnwrap(indexes.first)
        first.complete(store(path: volume.path))
        catalog.prepareToEject(device: "disk7")
        try waitUntil("ejected drive row") { catalog.rows.first?.status == .offline }
        catalog.prepareToEject(device: "unrelated-disk")
        catalog.prepareToUnmount(uuid: secondUUID)
        catalog.prepareToEject(device: "disk7")
        catalog.prepareToUnmount(uuid: uuid)
        XCTAssertEqual(first.stops, 1)
        // This is a negative temporal guarantee, so verify it throughout the observation window.
        try assertRemains("A slow eject must not restart indexing", duration: 2.1) { indexes.count == 1 && first.stops == 1 }
        XCTAssertEqual(indexes.count, 1, "A still-mounted path cannot prove a slow eject request has finished")
        catalog.setEnabled(false); catalog.setEnabled(true)
        try waitUntil("drive to restart after a cancelled eject") { indexes.count == 2 && indexes.last?.starts == 1 }
        XCTAssertEqual(indexes.count, 2)
        XCTAssertEqual(try XCTUnwrap(indexes.dropFirst().first).starts, 1, "A deliberate master switch restores a drive after another app rejects eject")
    }

    func testRemoveDeletesCatalogAndOnlyRecreatesAfterNextConnection() throws {
        let directory = temporaryDirectory(); var indexes: [Index] = []
        let catalog = ExternalDriveCatalog(directory: directory, monitor: Monitor(),
                                          initialPreferences: .init(enabled: true), makeIndex: { _, _, _ in let index = Index(); indexes.append(index); return index })
        defer { catalog.stop() }
        catalog.updateMounted([volume])
        let first = try XCTUnwrap(indexes.first)
        first.complete(store(path: volume.path))
        let persistence = ExternalDrivePersistence(directory: directory)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        try Data().write(to: persistence.indexURL(uuid))
        catalog.remove(uuid)
        XCTAssertTrue(catalog.rows.isEmpty); XCTAssertTrue(catalog.searchSources().isEmpty)
        XCTAssertEqual(first.stops, 1); XCTAssertFalse(FileManager.default.fileExists(atPath: persistence.indexURL(uuid).path))
        catalog.updateMounted([volume]); XCTAssertTrue(catalog.rows.isEmpty)
        catalog.updateMounted([]); catalog.updateMounted([volume])
        XCTAssertEqual(catalog.rows.count, 1); XCTAssertEqual(indexes.count, 2)
        L10n.snapshotChinese = true
        XCTAssertEqual(L10n.text("driveRemoveTitle", "T7 Shield"), "移除「T7 Shield」？")
        L10n.snapshotChinese = false
        XCTAssertEqual(L10n.text("driveRemoveTitle", "T7 Shield"), "Remove “T7 Shield”?")
        L10n.snapshotChinese = nil
    }

    func testProgressDoesNotInvalidateSearchUntilSourceStoreOrOnlineStateChanges() throws {
        var indexes: [Index] = [], changes = 0
        let catalog = ExternalDriveCatalog(directory: temporaryDirectory(), monitor: Monitor(),
                                          initialPreferences: .init(enabled: true), makeIndex: { _, _, _ in let index = Index(); indexes.append(index); return index })
        defer { catalog.stop() }
        catalog.onSourcesChange = { changes += 1 }
        catalog.updateMounted([volume]); XCTAssertEqual(changes, 0)
        let first = try XCTUnwrap(indexes.first)
        first.scannedCount = 45_000; first.onStateChange?(.scanning)
        XCTAssertEqual(changes, 0)
        let current = store(path: volume.path); first.complete(current); XCTAssertEqual(changes, 1)
        first.onStateChange?(.ready); XCTAssertEqual(changes, 1)
        current.write { current.version += 1 }; first.onIndexChange?(); XCTAssertEqual(changes, 2)
        catalog.prepareToEject(device: "disk7")
        try waitUntil("source-withdrawal notification") { changes == 3 }
        XCTAssertEqual(changes, 3)
    }

    func testMasterAndDriveSwitchesPreserveSettingsAndDeleteOnlyDisabledCatalogs() throws {
        let directory = temporaryDirectory(), monitor = Monitor()
        var indexes: [Index] = []
        let catalog = ExternalDriveCatalog(directory: directory, monitor: monitor, initialPreferences: .init(enabled: true), discover: { [self.volume] },
                                          makeIndex: { _, _, _ in let index = Index(); indexes.append(index); return index })
        defer { catalog.stop() }
        catalog.updateMounted([volume])
        let first = try XCTUnwrap(indexes.first)
        first.complete(store(path: volume.path))
        let db = ExternalDrivePersistence(directory: directory).indexURL(uuid)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        try Data("catalog".utf8).write(to: db)
        catalog.setEnabled(false)
        XCTAssertTrue(catalog.searchSources().isEmpty); XCTAssertEqual(first.stops, 1)
        XCTAssertTrue(FileManager.default.fileExists(atPath: db.path))
        catalog.setEnabled(true)
        try waitUntil("index recreation after re-enabling") { indexes.count == 2 && indexes.last?.starts == 1 }
        XCTAssertEqual(indexes.count, 2); XCTAssertTrue(catalog.searchSources().isEmpty)
        try XCTUnwrap(indexes.dropFirst().first).complete(store(path: volume.path))
        catalog.setDriveEnabled(uuid, false)
        XCTAssertTrue(catalog.searchSources().isEmpty); XCTAssertEqual(try XCTUnwrap(catalog.rows.first).status, .disabled)
        XCTAssertFalse(FileManager.default.fileExists(atPath: db.path))
        catalog.setDriveEnabled(uuid, true); XCTAssertEqual(indexes.count, 3)
    }

    func testDisconnectRetainsCatalogAndReconnectUsesSameUUIDAcrossRename() throws {
        let directory = temporaryDirectory()
        var indexes: [Index] = [], urls: [URL] = []
        let catalog = ExternalDriveCatalog(directory: directory, monitor: Monitor(), initialPreferences: .init(enabled: true),
                                          makeIndex: { _, url, _ in urls.append(url); let index = Index(); indexes.append(index); return index })
        defer { catalog.stop() }
        catalog.updateMounted([volume])
        let first = try XCTUnwrap(indexes.first)
        first.complete(store(path: volume.path))
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let originalURL = try XCTUnwrap(urls.first)
        try Data("existing catalog".utf8).write(to: originalURL)
        catalog.updateMounted([])
        XCTAssertTrue(catalog.searchSources().isEmpty); XCTAssertEqual(indexes.count, 1)
        XCTAssertEqual(catalog.knownDrives.first?.isConnected, false)
        XCTAssertEqual(try Data(contentsOf: originalURL), Data("existing catalog".utf8))
        var renamed = volume; renamed.name = "Renamed"; renamed.path = "/Volumes/Renamed 1"
        catalog.updateMounted([renamed]); XCTAssertEqual(indexes.count, 2); XCTAssertEqual(originalURL, try XCTUnwrap(urls.dropFirst().first))
        XCTAssertTrue(catalog.searchSources().isEmpty, "An old mount path never becomes actionable")
        try XCTUnwrap(indexes.dropFirst().first).complete(store(path: renamed.path))
        XCTAssertEqual(catalog.searchSources().first?.displayName, "Renamed")
        XCTAssertEqual(catalog.searchSources().first?.store.rootPath, renamed.path)
    }

    func testUpgradeLoadsOldPreferencesAndCatalogWithoutRecreatingThem() throws {
        let suite = "OilFindExternalSavedUpgrade-" + UUID().uuidString
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let directory = temporaryDirectory(), persistence = ExternalDrivePersistence(directory: temporaryDirectory())
        XCTAssertFalse(ExternalDrivePreferences().enabled)
        let legacy = ExternalDrivePersistence(directory: directory)
        let disabled = ExternalDriveRecord(id: secondUUID, name: "Disabled", path: "/Volumes/Disabled", lastConnected: Date(), enabled: false)
        legacy.save(.init(enabled: true, drives: [.init(id: uuid, name: volume.name, path: volume.path, lastConnected: Date()), disabled]))
        try Data("legacy catalog".utf8).write(to: legacy.indexURL(uuid))
        var urls: [URL] = []
        let catalog = ExternalDriveCatalog(directory: directory, monitor: Monitor(), discover: { [self.volume] },
                                          settingsDefaults: defaults, makeIndex: { _, url, _ in urls.append(url); return Index() })
        defer { catalog.stop() }
        catalog.start()
        try waitUntil("legacy preferences and mounted drive index") { catalog.ready && urls.count == 1 && catalog.rows.count == 2 }
        XCTAssertTrue(catalog.ready); XCTAssertTrue(catalog.enabled)
        XCTAssertEqual(catalog.rows.count, 2); XCTAssertEqual(catalog.rows.first { $0.id == secondUUID }?.status, .disabled)
        XCTAssertEqual(urls, [legacy.indexURL(uuid)])
        XCTAssertEqual(try Data(contentsOf: try XCTUnwrap(urls.first)), Data("legacy catalog".utf8))
        XCTAssertFalse(persistence.load().enabled)
    }

    func testPublicSnapshotArgumentsHaveThreeRowsAndNoPersistentIO() throws {
        let directory = temporaryDirectory()
        let catalog = ExternalDriveCatalog(directory: directory, monitor: Monitor(), discover: { XCTFail("Snapshot discovery"); return [] })
        var arguments = ["--snapshot", "/tmp/settings.png", "--external-drives-sample", "--allow-external-disk-images"]
        try catalog.prepareArguments(&arguments)
        XCTAssertEqual(arguments, ["--snapshot", "/tmp/settings.png"])
        XCTAssertEqual(catalog.rows.map(\.status), [.connected, .indexing, .offline])
        XCTAssertEqual(catalog.knownDrives.map(\.isConnected), [true, true, false])
        XCTAssertTrue(catalog.allowDiskImages); XCTAssertTrue(catalog.searchSources().isEmpty)
        catalog.start(); catalog.stop(); XCTAssertFalse(FileManager.default.fileExists(atPath: directory.path))
    }

    func testDisconnectedNotificationWaitsForApprovalCatalogSave() throws {
        let index = Index(), catalog = ExternalDriveCatalog(directory: temporaryDirectory(), monitor: Monitor(), initialPreferences: .init(enabled: true),
                                                            makeIndex: { _, _, _ in index })
        defer { catalog.stop() }
        catalog.updateMounted([volume]); index.complete(store(path: volume.path))
        var saved = false, disconnected = false
        let observation = catalog.changes.sink {
            if catalog.knownDrives.first?.isConnected == false {
                XCTAssertTrue(saved, "An extension must never load a catalog while stop is saving")
                disconnected = true
            }
        }
        defer { observation.cancel() }
        defer { index.onStop = nil }
        index.onStop = {
            XCTAssertTrue(catalog.searchSources().isEmpty, "Sources are withdrawn synchronously")
            index.onStateChange?(.idle)
            XCTAssertTrue(catalog.knownDrives.first?.isConnected == true)
            saved = true
        }
        catalog.prepareToUnmount(uuid: uuid)
        try waitUntil("post-save disconnection notification") { disconnected }
        XCTAssertTrue(disconnected)
    }

    func testReenableDiscardsStaleDiscoveryAndStoppedIndexCallbacks() throws {
        let discoveryEntered = expectation(description: "First discovery entered")
        let releaseDiscovery = DispatchSemaphore(value: 0), lock = NSLock()
        let current = volume
        var stale = current; stale.name = "Stale"; stale.path = "/Volumes/Stale"
        let staleVolume = stale
        var discoveries = 0, indexes: [Index] = [], changes = 0
        let catalog = ExternalDriveCatalog(directory: temporaryDirectory(), monitor: Monitor(), initialPreferences: .init(enabled: true),
                                          discover: {
            lock.lock(); discoveries += 1; let invocation = discoveries; lock.unlock()
            if invocation == 1 {
                discoveryEntered.fulfill()
                _ = releaseDiscovery.wait(timeout: .now() + 10)
                return [staleVolume]
            }
            return [current]
        }, makeIndex: { _, _, _ in let index = Index(); indexes.append(index); return index })
        defer { releaseDiscovery.signal(); catalog.stop() }
        catalog.onSourcesChange = { changes += 1 }
        catalog.updateMounted([current])
        let first = try XCTUnwrap(indexes.first)
        first.complete(store(path: current.path))
        catalog.start()
        let entered = XCTWaiter.wait(for: [discoveryEntered], timeout: 10)
        _ = try XCTUnwrap(entered == .completed ? true : nil, "First discovery must enter before toggling")
        catalog.setEnabled(false); catalog.setEnabled(true)
        XCTAssertEqual(first.stops, 1)
        XCTAssertEqual(indexes.count, 1, "Re-enabling must await fresh discovery")
        let disabledChanges = changes
        first.onStateChange?(.idle); first.onIndexChange?()
        XCTAssertEqual(changes, disabledChanges)
        XCTAssertTrue(catalog.searchSources().isEmpty)
        releaseDiscovery.signal()
        try waitUntil("fresh discovery to recreate the index") { indexes.count == 2 && indexes.last?.starts == 1 }
        let replacement = try XCTUnwrap(indexes.dropFirst().first)
        XCTAssertEqual(catalog.rows.first?.record.name, current.name)
        XCTAssertEqual(catalog.rows.first?.record.path, current.path)
        replacement.complete(store(path: current.path))
        let activeChanges = changes
        first.onStateChange?(.idle); first.onIndexChange?()
        XCTAssertEqual(changes, activeChanges)
        XCTAssertTrue(catalog.searchSources().first?.store === replacement.store)
    }

}
