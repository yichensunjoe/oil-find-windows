import XCTest
import OilFindCore
@testable import OilFindApp

final class OfflineUpdateTests: XCTestCase {
    func testOnlineUpdatesAreDisallowed() {
        XCTAssertFalse(UpdateManager.onlineUpdatesAllowed)
        let model = UpdateManager(snapshot: true)
        XCTAssertFalse(model.automatic)
        XCTAssertFalse(model.canCheck)
    }

    func testPreviouslyEnabledPreferenceCannotEnableTheUpdater() throws {
        let suite = "OilFindOfflineTests-" + UUID().uuidString
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let preferences = UpdatePreferences(defaults: defaults)
        preferences.automatic = true
        let model = UpdateManager(preferences: preferences)
        XCTAssertFalse(model.automatic)
        XCTAssertFalse(model.canCheck)
        XCTAssertTrue(preferences.automatic, "Initialization must not rewrite another installation's preferences.")
    }

    func testUpdateActionsRemainIdleInReleaseAndDebug() {
        let model = UpdateManager(snapshot: true)
        model.start()
        model.check()
        model.install()
        model.openWebsite()
        XCTAssertEqual(model.state, .idle)
        XCTAssertNil(model.manifest)
        XCTAssertFalse(model.preventsTermination)
        model.stop()
    }
}
