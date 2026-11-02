import XCTest
import OilFindCore
@testable import OilFindApp

final class OfflineUpdateTests: XCTestCase {
    private func isolatedPreferences() throws -> UpdatePreferences {
        let suite = "OilFindOfflineTests-" + UUID().uuidString
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        addTeardownBlock { defaults.removePersistentDomain(forName: suite) }
        return UpdatePreferences(defaults: defaults)
    }

    func testOnlineUpdatesAreDisallowed() throws {
        XCTAssertFalse(UpdateManager.onlineUpdatesAllowed)
        let model = UpdateManager(snapshot: true, preferences: try isolatedPreferences())
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

    #if DEBUG
    func testDebugUpdateExercisesAreRejectedWithoutNetworkOrGUI() {
        for flag in ["--update-e2e", "--update-e2e-restarted"] {
            XCTAssertTrue(UpdateExercise.handles([flag, "/unused"]))
            XCTAssertThrowsError(try UpdateExercise.start([flag, "/unused"])) { error in
                XCTAssertTrue(String(describing: error).contains("disabled in offline builds"))
            }
        }
    }
    #endif

    func testUpdateActionsRemainIdleInReleaseAndDebug() throws {
        let model = UpdateManager(snapshot: true, preferences: try isolatedPreferences())
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
