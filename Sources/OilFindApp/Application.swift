import AppKit
import OilFindCore

public enum Application {
    static let preferenceDomain = "com.reasonix.oilfind.intranet"
    static let preferences = UserDefaults(suiteName: preferenceDomain)!
    static let supportDirectory = FileManager.default.homeDirectoryForCurrentUser
        .appendingPathComponent("Library/Application Support/Oil Find Intranet", isDirectory: true)
    static let cacheDirectory = FileManager.default.homeDirectoryForCurrentUser
        .appendingPathComponent("Library/Caches/Oil Find Intranet", isDirectory: true)

    public static func run(appExtension: ApplicationExtension? = nil, arguments: [String] = Array(CommandLine.arguments.dropFirst())) {
#if DEBUG && OILFIND_OFFLINE_BUILD
    if UpdateExercise.handles(arguments) {
        fputs("Oil Find Intranet: online update exercises are disabled in offline builds.\n", stderr)
        exit(1)
    }
#endif
    if let status = appExtension?.handleLaunch(arguments: arguments) { exit(status) }
    let externalDrives = ExternalDriveCatalog()
#if DEBUG
    var arguments = arguments
    do { try externalDrives.prepareArguments(&arguments); try appExtension?.prepareArguments(&arguments) }
    catch { fputs("Oil Find arguments: \(error)\n", stderr); exit(1) }
    if arguments.contains("--snapshot") {
        appExtension?.attachExternalDrives(externalDrives)
        do { try Snapshot.run(arguments, appExtension: appExtension, externalDrives: externalDrives); exit(0) }
        catch { fputs("Oil Find snapshot: \(error)\n", stderr); exit(1) }
    }
    if UpdateExercise.handles(arguments) {
        do { try UpdateExercise.start(arguments); NSApplication.shared.run(); exit(0) }
        catch { fputs("Oil Find update exercise: \(error)\n", stderr); exit(1) }
    }
#endif
#if !OILFIND_OFFLINE_BUILD
    SystemUpdateFiles().registerLaunch(application: Bundle.main.bundleURL, current: UpdateManager.current)
#endif
    let app = NSApplication.shared
    SettingsPreferences.register(in: preferences)
    let delegate = AppDelegate(appExtension: appExtension, externalDrives: externalDrives)
    app.delegate = delegate
    app.run()
    }
}
