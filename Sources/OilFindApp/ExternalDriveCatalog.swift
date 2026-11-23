import AppKit
import Combine
import OilFindCore

struct ExternalDriveRecord: Codable, Equatable, Identifiable {
    var id: String
    var name: String
    var path: String
    var lastConnected: Date
    var enabled = true
    // Earlier mount names are kept with the UUID, not written into the user's exclusions.
    var previousPaths: [String]? = nil
}

struct ExternalExclusionBinding: Codable, Equatable {
    var id: String
    var root: String
}

struct ExternalDrivePreferences: Codable {
    var enabled = false
    var drives: [ExternalDriveRecord] = []
    var exclusionBindings: [String: ExternalExclusionBinding]? = nil
}

/// Only validated UUIDs are used as file names; the catalog survives volume renames.
struct ExternalDrivePersistence {
    var directory: URL
    var metadataURL: URL { directory.appendingPathComponent("drives.json") }
    func indexURL(_ uuid: String) -> URL {
        precondition(UUID(uuidString: uuid) != nil)
        return directory.appendingPathComponent(uuid.uppercased() + ".oilfind")
    }
    func load() -> ExternalDrivePreferences {
        guard let data = try? Data(contentsOf: metadataURL),
              var preferences = try? JSONDecoder().decode(ExternalDrivePreferences.self, from: data) else { return .init() }
        var seen = Set<String>()
        preferences.drives = preferences.drives.compactMap { record in
            guard UUID(uuidString: record.id) != nil, record.path.hasPrefix("/"), !record.name.isEmpty else { return nil }
            var record = record; record.id = record.id.uppercased()
            return seen.insert(record.id).inserted ? record : nil
        }
        return preferences
    }
    func save(_ preferences: ExternalDrivePreferences) {
        do {
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true,
                                                     attributes: [.posixPermissions: 0o700])
            try JSONEncoder().encode(preferences).write(to: metadataURL, options: .atomic)
        } catch { }
    }
    func remove(_ uuid: String) { try? FileManager.default.removeItem(at: indexURL(uuid)) }
}

protocol ExternalDriveIndex: AnyObject {
    var store: IndexStore? { get }
    var state: IndexManager.State { get }
    var scannedCount: Int { get }
    var onStateChange: ((IndexManager.State) -> Void)? { get set }
    var onIndexChange: (() -> Void)? { get set }
    func start()
    func stop()
    func updateConfig(_ config: IndexConfig)
}
extension IndexManager: ExternalDriveIndex { }

final class ExternalDriveCatalog: ObservableObject, ExternalDriveCatalogReading {
    private static let indexingDisabledKey = "externalDriveIndexingDisabled"
    private static let disabledDriveIDsKey = "disabledExternalDriveUUIDs"

    struct Row: Identifiable {
        enum Status { case connected, indexing, offline, disabled, exclusionReview }
        var record: ExternalDriveRecord
        var status: Status
        var count: Int
        var id: String { record.id }
    }
    private struct SourceSignature: Equatable {
        var id: String
        var name: String
        var online: Bool
        var store: ObjectIdentifier
        var version: UInt64
    }
    private var lastSourceSignature: [SourceSignature] = []
    private struct Session {
        var index: ExternalDriveIndex
        var path: String
        var device: String
        var online: Bool
    }
    @Published private(set) var enabled = false
    @Published private(set) var ready = false
    @Published private(set) var rows: [Row] = []
    private var discovered = false
    private var preferences = ExternalDrivePreferences()
    private var volumes: [String: ExternalVolume] = [:]
    private var removedUntilDisconnect = Set<String>()
    private var sessions: [String: Session] = [:]
    private let sessionLock = NSLock()
    private var ejectingDevices = Set<String>()
    private var unmountingVolumes = Set<String>()
    private var approvalsInProgress = Set<String>()
    private let discoveryQueue = DispatchQueue(label: "com.oiloil.find.external-discovery", qos: .utility)
    private let ioQueue = DispatchQueue(label: "com.oiloil.find.external-catalog", qos: .utility)
    private let persistence: ExternalDrivePersistence
    private let catalogChanges = PassthroughSubject<Void, Never>()
    private var lastKnownDrives: [ExternalDriveInfo] = []
    private let monitor: ExternalDriveMonitoring
    private let discover: () -> [ExternalVolume]
    private let settingsDefaults: UserDefaults
    private let makeIndex: (IndexConfig, URL, String) -> ExternalDriveIndex
    private var started = false, stopped = false, monitoring = false, discovering = false, discoveryPending = false
    private var discoveryGeneration = 0
    private var progressTimer: Timer?
    var onSourcesChange: (() -> Void)?
    #if DEBUG
    private(set) var snapshot = false
    var allowDiskImages = false
    #endif

    init(directory: URL = Application.supportDirectory.appendingPathComponent("external-drives", isDirectory: true),
         monitor: ExternalDriveMonitoring = ExternalVolumeMonitor(),
         initialPreferences: ExternalDrivePreferences? = nil,
         discover: @escaping () -> [ExternalVolume] = ExternalVolumeMonitor.mounted,
         settingsDefaults: UserDefaults = Application.preferences,
         makeIndex: ((IndexConfig, URL, String) -> ExternalDriveIndex)? = nil) {
        persistence = ExternalDrivePersistence(directory: directory)
        self.monitor = monitor; self.discover = discover; self.settingsDefaults = settingsDefaults
        self.makeIndex = makeIndex ?? { IndexManager(config: $0, dbURL: $1, volumeUUID: $2, settingsDefaults: settingsDefaults) }
        if let initialPreferences { restorePreferences(initialPreferences) }
        monitor.onChange = { [weak self] in DispatchQueue.main.async { self?.discoverVolumes() } }
        monitor.beforeEject = { [weak self] device in self?.prepareToEject(device: device) }
        monitor.beforeUnmount = { [weak self] uuid in self?.prepareToUnmount(uuid: uuid) }
    }
    private func withSessions<T>(_ body: () -> T) -> T {
        sessionLock.lock(); defer { sessionLock.unlock() }; return body()
    }
    var changes: AnyPublisher<Void, Never> { catalogChanges.eraseToAnyPublisher() }
    var knownDrives: [ExternalDriveInfo] {
        guard usable, discovered else { return [] }
        let blocked = exclusionReviewRequired
        return preferences.drives.map { record in
            let volume = volumes[record.id]
            let connected = withSessions {
                approvalsInProgress.contains(record.id) || (volume.map {
                    !unmountingVolumes.contains(record.id) && !ejectingDevices.contains($0.device)
                } ?? false)
            }
            return ExternalDriveInfo(id: record.id, name: record.name, path: record.path,
                                     enabled: record.enabled && !blocked, isConnected: connected, catalogURL: persistence.indexURL(record.id))
        }
    }
    var usable: Bool { enabled && ready && !stopped }
    private var disabledDriveIDs: Set<String> {
        Set(settingsDefaults.stringArray(forKey: Self.disabledDriveIDsKey) ?? [])
    }
    private func restorePreferences(_ loaded: ExternalDrivePreferences) {
        // A failed catalog write can leave an old opt-in on disk. The isolated
        // application preferences retain revocations independently of the catalog.
        preferences = loaded
        preferences.enabled = loaded.enabled && !settingsDefaults.bool(forKey: Self.indexingDisabledKey)
        let disabledIDs = disabledDriveIDs
        for i in preferences.drives.indices where disabledIDs.contains(preferences.drives[i].id) {
            preferences.drives[i].enabled = false
        }
        enabled = preferences.enabled; ready = true
    }
    func start() {
        guard !started else { return }; started = true
        #if DEBUG
        if snapshot { publish(); return }
        #endif
        if ready { configurationChanged(); return }
        let persistence = self.persistence
        ioQueue.async { [weak self] in
            let loaded = persistence.load()
            DispatchQueue.main.async { [weak self] in
                guard let self, !self.stopped else { return }
                self.restorePreferences(loaded)
                self.configurationChanged()
            }
        }
    }
    func stop() {
        stopped = true; discoveryGeneration += 1
        progressTimer?.invalidate(); progressTimer = nil
        if monitoring { monitor.stop(); monitoring = false }
        let current = withSessions { let value = sessions; sessions.removeAll(); return value }
        current.values.forEach { $0.index.stop() }
        notifySources()
    }
    deinit {
        progressTimer?.invalidate(); monitor.stop()
        sessions.values.forEach { $0.index.stop() }
    }
    private func configurationChanged() {
        #if DEBUG
        if snapshot { publish(); return }
        #endif
        if usable {
            if !monitoring { monitor.start(); monitoring = true }
            discoverVolumes()
        } else {
            if monitoring { monitor.stop(); monitoring = false }
            discoveryGeneration += 1
            // Mounts can change while monitoring is suspended; rediscover before going online.
            volumes.removeAll(); discovered = false
            withSessions { ejectingDevices.removeAll(); unmountingVolumes.removeAll() }
        }
        reconcile()
    }
    private func persist() {
        #if DEBUG
        if snapshot { return }
        #endif
        let preferences = self.preferences, persistence = self.persistence
        ioQueue.async { persistence.save(preferences) }
    }
    private func persistIndexingPreference(_ value: Any, forKey key: String) {
        #if DEBUG
        if snapshot { return }
        #endif
        settingsDefaults.set(value, forKey: key)
    }
    private func setDriveOptOut(_ uuid: String, disabled: Bool) {
        var disabledIDs = disabledDriveIDs
        guard disabledIDs.contains(uuid) != disabled else { return }
        if disabled {
            disabledIDs.insert(uuid)
        } else {
            disabledIDs.remove(uuid)
        }
        persistIndexingPreference(disabledIDs.sorted(), forKey: Self.disabledDriveIDsKey)
    }
    func setEnabled(_ value: Bool) {
        guard ready, value != enabled else { return }
        persistIndexingPreference(!value, forKey: Self.indexingDisabledKey)
        enabled = value; preferences.enabled = value; persist(); configurationChanged()
    }
    func setDriveEnabled(_ uuid: String, _ value: Bool) {
        guard usable, let i = preferences.drives.firstIndex(where: { $0.id == uuid }), preferences.drives[i].enabled != value else { return }
        setDriveOptOut(uuid, disabled: !value)
        preferences.drives[i].enabled = value
        if !value { discardIndex(uuid) }
        persist(); reconcile()
    }
    func remove(_ uuid: String) {
        guard usable, preferences.drives.contains(where: { $0.id == uuid }) else { return }
        discardIndex(uuid); preferences.drives.removeAll { $0.id == uuid }
        // An explicit removal resets the drive's setting for its next connection.
        setDriveOptOut(uuid, disabled: false)
        // Keep the mounted volume suppressed until its next mount notification.
        removedUntilDisconnect.insert(uuid)
        volumes.removeValue(forKey: uuid)
        persist(); reconcile()
    }
    func confirmRemove(_ row: Row, window: NSWindow?) {
        guard usable, let window else { return }
        let alert = NSAlert()
        alert.messageText = L10n.text("driveRemoveTitle", row.record.name)
        alert.informativeText = L10n.text("driveRemoveBody")
        alert.addButton(withTitle: L10n.text("driveRemove"))
        alert.addButton(withTitle: L10n.text("action.cancel"))
        alert.beginSheetModal(for: window) { [weak self] response in
            if response == .alertFirstButtonReturn { self?.remove(row.id) }
        }
    }
    private func discardIndex(_ uuid: String) {
        let index = withSessions { sessions.removeValue(forKey: uuid)?.index }
        index?.stop()
        #if DEBUG
        if snapshot { return }
        #endif
        persistence.remove(uuid)
    }
    func discoverVolumes() {
        guard usable else { return }
        if discovering { discoveryPending = true; return }
        discovering = true
        let generation = discoveryGeneration, discover = self.discover
        discoveryQueue.async { [weak self] in
            let mounted = discover()
            DispatchQueue.main.async { [weak self] in
                guard let self else { return }
                self.discovering = false
                if self.usable, self.discoveryGeneration == generation { self.updateMounted(mounted) }
                if self.discoveryPending { self.discoveryPending = false; self.discoverVolumes() }
            }
        }
    }
    func updateMounted(_ mounted: [ExternalVolume], now: Date = Date()) {
        #if DEBUG
        let allowImages = allowDiskImages
        #else
        let allowImages = false
        #endif
        let candidates = mounted.filter { $0.eligible(allowDiskImages: allowImages) }
        removedUntilDisconnect.formIntersection(Set(candidates.map { $0.uuid.uppercased() }))
        let eligible = candidates.filter { !removedUntilDisconnect.contains($0.uuid.uppercased()) }
        discovered = true
        let previous = volumes
        volumes = Dictionary(eligible.map { ($0.uuid.uppercased(), $0) }, uniquingKeysWith: { first, _ in first })
        withSessions {
            ejectingDevices.formIntersection(Set(eligible.map(\.device)))
            unmountingVolumes.formIntersection(Set(eligible.map { $0.uuid.uppercased() }))
        }
        let disabledIDs = disabledDriveIDs
        for (uuid, volume) in volumes {
            if let i = preferences.drives.firstIndex(where: { $0.id == uuid }) {
                let oldPath = preferences.drives[i].path
                if oldPath != volume.path, let old = ExcludedFolders.normalized(oldPath), old != "/" {
                    var paths = preferences.drives[i].previousPaths ?? []
                    if !paths.contains(old) { paths.append(old) }
                    if let current = ExcludedFolders.normalized(volume.path) { paths.removeAll { $0 == current } }
                    preferences.drives[i].previousPaths = paths.isEmpty ? nil : paths
                }
                preferences.drives[i].name = volume.name; preferences.drives[i].path = volume.path
                if previous[uuid] == nil || previous[uuid]?.path != volume.path { preferences.drives[i].lastConnected = now }
            } else {
                preferences.drives.append(.init(id: uuid, name: volume.name, path: volume.path, lastConnected: now,
                                               enabled: !disabledIDs.contains(uuid)))
            }
        }
        persist(); reconcile()
    }
    private var userExcludedPaths: [String] {
        ExcludedFolders.adding(settingsDefaults.stringArray(forKey: "userExcludedPaths") ?? [], to: [])
    }
    private func bindings(for path: String) -> [(id: String, root: String)] {
        if let binding = preferences.exclusionBindings?[path] {
            guard let record = preferences.drives.first(where: { $0.id == binding.id.uppercased() }),
                  let root = ExcludedFolders.normalized(binding.root), root != "/", root != "/Volumes",
                  ExcludedFolders.contains(root, path: path),
                  ([record.path] + (record.previousPaths ?? [])).contains(where: { ExcludedFolders.normalized($0) == root }) else { return [] }
            return [(record.id, root)]
        }
        var result: [(id: String, root: String)] = []
        for record in preferences.drives {
            var seen = Set<String>()
            for value in [record.path] + (record.previousPaths ?? []) {
                guard let root = ExcludedFolders.normalized(value), root != "/", root != "/Volumes", seen.insert(root).inserted,
                      ExcludedFolders.contains(root, path: path) else { continue }
                result.append((record.id, root))
            }
        }
        return result
    }
    var exclusionReviewRequired: Bool {
        userExcludedPaths.contains { path in
            let externalPath = path != "/Volumes" && ExcludedFolders.contains("/Volumes", path: path)
            return (externalPath || preferences.exclusionBindings?[path] != nil) && bindings(for: path).count != 1
        }
    }
    private func mappedExclusions(for record: ExternalDriveRecord, roots: [String]) -> [String] {
        let originals = userExcludedPaths
        var exclusions = originals
        for original in originals {
            let matches = bindings(for: original)
            guard matches.count == 1, matches[0].id == record.id else { continue }
            for value in roots {
                guard let root = ExcludedFolders.normalized(value), root != "/", root != "/Volumes",
                      root != matches[0].root else { continue }
                exclusions.append(root + original.dropFirst(matches[0].root.count))
            }
        }
        return ExcludedFolders.adding(exclusions, to: [])
    }
    private func config(for record: ExternalDriveRecord) -> IndexConfig {
        var config = SettingsPreferences.indexConfig(limited: false, in: settingsDefaults)
        let path = ExcludedFolders.normalized(record.path) ?? record.path
        config.rootPath = path
        // The startup-volume exclusion of /Volumes must not exclude this volume itself.
        config.excludedPaths.removeAll { ExcludedFolders.contains($0, path: path) }
        config.userExcludedPaths = mappedExclusions(for: record, roots: [path])
        return config
    }
    func filteredExtensionSources(_ sources: [SearchSource]) -> [SearchSource] {
        guard !exclusionReviewRequired else { return [] }
        let exclusions = userExcludedPaths + preferences.drives.flatMap { mappedExclusions(for: $0, roots: [$0.path] + ($0.previousPaths ?? [])) }
        return sources.filter { source in
            let root = source.store.read { source.store.rootPath }
            return !exclusions.contains { ExcludedFolders.contains(root, path: $0) || ExcludedFolders.contains($0, path: root) }
        }
    }
    @discardableResult func bindSelectedExclusions(_ paths: [String]) -> Bool {
        var saved = preferences.exclusionBindings ?? [:], changed = false
        for value in paths {
            guard let path = ExcludedFolders.normalized(value) else { continue }
            let candidates: [ExternalExclusionBinding] = volumes.compactMap { entry in
                let uuid = entry.key, volume = entry.value
                guard let root = ExcludedFolders.normalized(volume.path), root != "/", root != "/Volumes",
                      ExcludedFolders.contains(root, path: path),
                      withSessions({ !unmountingVolumes.contains(uuid) && !ejectingDevices.contains(volume.device) }) else { return nil }
                return ExternalExclusionBinding(id: uuid, root: root)
            }
            let depth = candidates.map { $0.root.count }.max()
            let closest = candidates.filter { $0.root.count == depth }
            if closest.count == 1 {
                if saved[path] != closest[0] { saved[path] = closest[0]; changed = true }
            } else if saved.removeValue(forKey: path) != nil { changed = true }
        }
        if changed { preferences.exclusionBindings = saved.isEmpty ? nil : saved; persist() }
        return changed
    }
    func updateSettings() {
        if let saved = preferences.exclusionBindings {
            let active = Set(userExcludedPaths), retained = saved.filter { active.contains($0.key) }
            if retained.count != saved.count { preferences.exclusionBindings = retained.isEmpty ? nil : retained; persist() }
        }
        if !exclusionReviewRequired {
            let current = withSessions { sessions.map { ($0.key, $0.value.index) } }
            for (id, index) in current {
                if let record = preferences.drives.first(where: { $0.id == id }) { index.updateConfig(config(for: record)) }
            }
        }
        reconcile()
    }
    private func reconcile() {
        #if DEBUG
        if snapshot { publish(); return }
        #endif
        let blocked = exclusionReviewRequired
        for record in preferences.drives {
            let volume = volumes[record.id]
            let suppressed = withSessions { unmountingVolumes.contains(record.id) || (volume.map { ejectingDevices.contains($0.device) } ?? false) }
            let online = volume != nil && !suppressed
            let old = withSessions { sessions[record.id] }
            if !usable || !record.enabled || !online || blocked || old?.path != record.path {
                if let old {
                    if old.online { old.index.stop() }
                    _ = withSessions { sessions.removeValue(forKey: record.id) }
                }
            }
            guard usable, record.enabled, online, !blocked, withSessions({ sessions[record.id] == nil }) else { continue }
            let index = makeIndex(config(for: record), persistence.indexURL(record.id), record.id)
            index.onStateChange = { [weak self, weak index] _ in
                guard let self, let index, self.withSessions({ self.sessions[record.id]?.index === index }) else { return }
                self.publish()
            }
            index.onIndexChange = { [weak self, weak index] in
                guard let self, let index, self.withSessions({ self.sessions[record.id]?.index === index }) else { return }
                self.publish()
            }
            withSessions { sessions[record.id] = Session(index: index, path: record.path, device: volume?.device ?? "", online: online) }
            index.start()
        }
        publish()
    }
    /// Called synchronously from Disk Arbitration before its approval is returned.
    func prepareToEject(device: String) {
        prepareApproval {
            let affected = sessions.filter { $0.value.online && $0.value.device == device }
            if !affected.isEmpty { ejectingDevices.insert(device) }
            for uuid in affected.keys { sessions[uuid]?.online = false; approvalsInProgress.insert(uuid) }
            return affected
        }
    }
    func prepareToUnmount(uuid: String) {
        let uuid = uuid.uppercased()
        prepareApproval {
            let affected = sessions.filter { $0.key == uuid && $0.value.online }
            if !affected.isEmpty { unmountingVolumes.insert(uuid); sessions[uuid]?.online = false; approvalsInProgress.insert(uuid) }; return affected
        }
    }
    private func prepareApproval(_ affectedSessions: () -> [String: Session]) {
        let affected = withSessions(affectedSessions)
        guard !affected.isEmpty else { return }
        for session in affected.values { session.index.stop() }
        // State callbacks can run while stop is saving. Publish disconnection only after every catalog is complete.
        withSessions { approvalsInProgress.subtract(affected.keys) }
        DispatchQueue.main.async { [weak self] in
            guard let self, !self.stopped else { return }
            self.publish()
        }
    }
    func searchSources() -> [SearchSource] {
        guard usable, !exclusionReviewRequired else { return [] }
        return preferences.drives.filter(\.enabled).compactMap { record in
            let session = withSessions { sessions[record.id] }
            guard session?.online == true, let store = session?.index.store, store.rootPath == record.path else { return nil }
            let config = config(for: record)
            return SearchSource(id: record.id, displayName: record.name, isOnline: true, store: store,
                                explainPath: { Coverage.explain(path: $0, config: config, store: store) })
        }
    }
    private func notifySources() {
        let signature = searchSources().map { source in
            SourceSignature(id: source.id, name: source.displayName, online: source.isOnline,
                            store: ObjectIdentifier(source.store), version: source.store.read { source.store.version })
        }
        let known = knownDrives
        if known != lastKnownDrives { lastKnownDrives = known; catalogChanges.send() }
        guard signature != lastSourceSignature else { return }
        lastSourceSignature = signature; onSourcesChange?()
    }
    private func publish() {
        #if DEBUG
        if snapshot { objectWillChange.send(); notifySources(); return }
        #endif
        let blocked = exclusionReviewRequired
        rows = preferences.drives.sorted { $0.lastConnected > $1.lastConnected }.map { record in
            let session = withSessions { sessions[record.id] }
            let store = session?.index.store
            let count = store.map { value in value.read { max(0, value.liveCount - 1) } } ?? session?.index.scannedCount ?? 0
            let online = volumes[record.id].map { volume in
                withSessions { !unmountingVolumes.contains(record.id) && !ejectingDevices.contains(volume.device) }
            } ?? false
            let status: Row.Status = !record.enabled ? .disabled : blocked && usable && online ? .exclusionReview
                : session?.online != true ? .offline : session?.index.state == .ready ? .connected : .indexing
            return Row(record: record, status: status, count: count)
        }
        if rows.contains(where: { $0.status == .indexing }), progressTimer == nil {
            progressTimer = Timer.scheduledTimer(withTimeInterval: 0.5, repeats: true) { [weak self] _ in self?.publish() }
        } else if !rows.contains(where: { $0.status == .indexing }) {
            progressTimer?.invalidate(); progressTimer = nil
        }
        notifySources()
    }
    #if DEBUG
    func prepareArguments(_ arguments: inout [String]) throws {
        for key in ["--allow-external-disk-images", "--external-drives-sample"] {
            guard arguments.filter({ $0 == key }).count <= 1 else { throw SnapshotError.failed("\(key) may only occur once") }
        }
        if arguments.contains("--external-drives-sample"), !arguments.contains("--snapshot") {
            throw SnapshotError.failed("--external-drives-sample requires --snapshot")
        }
        allowDiskImages = arguments.contains("--allow-external-disk-images")
        if arguments.contains("--snapshot") { installSnapshot(sample: arguments.contains("--external-drives-sample")) }
        arguments.removeAll { $0 == "--allow-external-disk-images" || $0 == "--external-drives-sample" }
    }
    func installSnapshot(sample: Bool = true) {
        snapshot = true; ready = true; enabled = true; discovered = true
        guard sample else { return }
        let date = Date(timeIntervalSince1970: 1791532800)
        let sample: [(String, String, Row.Status, Int)] = [
            ("00000000-0000-0000-0000-000000000001", "T7 Shield", .connected, 1_234_567),
            ("00000000-0000-0000-0000-000000000002", "SD Card", .indexing, 45_000),
            ("00000000-0000-0000-0000-000000000003", "Archive", .offline, 854_320)]
        rows = sample.enumerated().map { i, item in
            Row(record: .init(id: item.0, name: item.1, path: "/Volumes/" + item.1, lastConnected: date.addingTimeInterval(-Double(i == 2 ? 86400 : i * 1800))), status: item.2, count: item.3)
        }
        preferences.drives = rows.map(\.record)
        volumes = Dictionary(uniqueKeysWithValues: rows.filter { $0.status != .offline }.map { ($0.id, ExternalVolume(uuid: $0.id, name: $0.record.name, path: $0.record.path)) })
        notifySources()
    }
    #endif
}
