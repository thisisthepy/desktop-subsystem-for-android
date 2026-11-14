import Foundation

/// The "Run as ARM" switch of an app, as `dsa apps list` reports it: whether the app runs its
/// ARM code now, whether the switch can be changed, and if not, why (`system-app`,
/// `no-arm-libraries`, `translation-off` or `arm-only`).
public struct RunAsArmSwitch: Equatable, Sendable {
    public var on: Bool
    public var available: Bool
    public var reason: String

    public init(on: Bool, available: Bool, reason: String) {
        self.on = on
        self.available = available
        self.reason = reason
    }

    /// The switch an older dsa says nothing of: off, and not to be changed.
    public static let unknown = RunAsArmSwitch(on: false, available: false, reason: "unknown")
}

/// How a change of the "Run as ARM" switch went: `status` is dsa's stable reason (`done`,
/// `data-would-be-erased`, `not-installed`, `system-app`, `no-libraries`, `translation-off` or
/// `other`), `message` Android's own words.
public struct RunAsArmReport: Equatable, Sendable {
    public var done: Bool
    public var status: String
    public var message: String
    public var on: Bool
}


/// How far an app may use root: su does not exist for it (`off`), each su call waits for an
/// administrator's approval (`ask`), or su is approved without asking (`remember`). The order
/// is the order the Apps page lists them.
public enum RootLevel: String, CaseIterable, Sendable {
    case off, ask, remember

    /// The level dsa named, or nil for `unavailable` (root elevation is off for the Mac) and
    /// for a dsa that does not say.
    public static func read(_ code: String) -> RootLevel? { RootLevel(rawValue: code) }

    /// Where the level is among the choices the Apps page offers.
    public var index: Int { RootLevel.allCases.firstIndex(of: self) ?? 0 }
}

/// How a change of an app's root level went: whether it was set, the level the app has now,
/// and the service's words when it was not set.
public struct RootReport: Equatable, Sendable {
    public var done: Bool
    public var level: String
    public var message: String
}

/// A root level the Apps page offers: what the list shows, and the level.
public struct RootChoice: Equatable, Hashable, Sendable {
    public var label: String
    public var level: RootLevel
}
/// One app installed in Android, as `dsa apps list` reports it.
public struct InstalledAppInfo: Equatable, Sendable {
    public var package, label, versionName: String
    public var versionCode: Int64
    public var firstInstallMs, lastUpdateMs: UInt64
    public var systemApp: Bool
    public var appBytes, dataBytes, cacheBytes: UInt64
    public var iconPng: Data
    public var runAsArm: RunAsArmSwitch = .unknown
    /// How far the app may use root, as dsa says it: `off`, `ask` or `remember`, or
    /// `unavailable` while root elevation is off for the Mac. Empty from a dsa that does not
    /// say.
    public var root = ""
    /// The user ID the app shares with other apps (`android:sharedUserId`); empty for an app
    /// with a user ID of its own, and from a dsa that does not say.
    public var sharedUserId = ""

    public var totalBytes: UInt64 { appBytes + dataBytes + cacheBytes }
}

/// How an app's window follows a move to a monitor of another scale. `auto` and `system` never
/// restart the app.
public enum DpiScaling: String, CaseIterable, Equatable, Sendable {
    /// App for apps that say they handle a change of density themselves, System for the others.
    case auto
    /// The app redraws at the new monitor's scale. Android recreates the screen of an app that
    /// does not say it handles a change of density, and restores its state.
    case app
    /// The app keeps its scale and its window is scaled to the new monitor, at the same
    /// physical size.
    case system
    /// The app gets the new scale without a restart even when it does not say it handles one.
    /// It needs a change to Android's framework; until a build has it, it works like `system`.
    case systemForced = "system-forced"

    /// The choice `dsa app-window` names; anything else, or nothing, is `auto`.
    public static func read(_ code: String?) -> DpiScaling {
        code.flatMap(DpiScaling.init(rawValue:)) ?? .auto
    }

    /// The key of its name in the string catalog.
    var textKey: String {
        switch self {
        case .auto: return "Apps_DpiScaling_Auto"
        case .app: return "Apps_DpiScaling_App"
        case .system: return "Apps_DpiScaling_System"
        case .systemForced: return "Apps_DpiScaling_SystemForced"
        }
    }
}

/// A scaling choice the Apps page offers: what the list shows, the choice, and whether it does
/// what it says yet.
public struct DpiScalingChoice: Equatable, Hashable, Sendable {
    public var label: String
    public var scaling: DpiScaling
    public var available: Bool
}

/// The user's window settings for one app, as `dsa app-window` reports them: whether its
/// windows keep their size, the size they open at in density-independent pixels (nil for the
/// size Android gives the task), and how its windows follow a monitor of another scale.
public struct AppWindowSetting: Equatable, Sendable {
    public var package: String
    public var fixedSize: Bool
    public var startWidth: Int?
    public var startHeight: Int?
    public var dpiScaling: DpiScaling = .auto

    public static func standard(_ package: String) -> AppWindowSetting {
        AppWindowSetting(package: package, fixedSize: false, startWidth: nil, startHeight: nil)
    }

    /// The start size as `dsa app-window --start-size` takes it.
    public var startSizeArgument: String {
        if let width = startWidth, let height = startHeight {
            return "\(width)x\(height)"
        }
        return "auto"
    }
}

/// A start size the Apps page offers, in density-independent pixels; nil for the size
/// Android gives the task.
public struct StartSizeChoice: Equatable, Hashable, Sendable {
    public var label: String
    public var width: Int?
    public var height: Int?
}

/// How an uninstall went: `status` is dsa's stable reason (`uninstalled`, `not-installed`,
/// `system-app`, `blocked` or `other`), `message` Android's own words.
public struct UninstallReport: Equatable, Sendable {
    public var uninstalled: Bool
    public var status: String
    public var message: String
}

/// Asks dsa about the apps installed in Android, and acts on them.
public final class AppsService {
    private let service: DsaService

    public init(cli: DsaCli) {
        service = DsaService(cli: cli)
    }

    static func readRunAsArm(_ app: Json) -> RunAsArmSwitch {
        let arm = app["run_as_arm"]
        guard arm.isObject else {
            return .unknown
        }
        return RunAsArmSwitch(on: arm.bool("on"), available: arm.bool("available"), reason: arm.string("reason"))
    }

    static func readApp(_ app: Json) -> InstalledAppInfo {
        InstalledAppInfo(
            package: app.string("package"), label: app.string("label"), versionName: app.string("version_name"),
            versionCode: app.int64("version_code"), firstInstallMs: app.uint("first_install_time_ms"),
            lastUpdateMs: app.uint("last_update_time_ms"), systemApp: app.bool("system_app"),
            appBytes: app.uint("app_bytes"), dataBytes: app.uint("data_bytes"), cacheBytes: app.uint("cache_bytes"),
            iconPng: app.base64("icon_png"), runAsArm: readRunAsArm(app), root: app.string("root"),
            sharedUserId: app.string("shared_user_id"))
    }

    /// Every app installed in Android, with its size read again; Android is started first when
    /// it is not running.
    public func list() async throws -> [InstalledAppInfo] {
        try await service.runJson(["apps", "list"])["apps"].items.map(AppsService.readApp).filter { !$0.package.isEmpty }
    }

    /// Opens the app in a window of its own.
    public func open(_ package: String) async throws { _ = try await service.runText(["launch", package]) }

    /// Puts a shortcut to `package` on the Desktop, which opens the app as Launchpad does and
    /// goes when the app is uninstalled.
    public func desktopShortcut(_ package: String) async throws {
        _ = try await service.runJson(["apps", "desktop-shortcut", package])
    }

    /// Opens Android's own settings of `package` (its permissions, storage and notifications),
    /// or Android's list of every app when it is nil.
    public func openSettings(_ package: String?) async throws {
        _ = try await service.runJson(["apps", "settings"] + (package.map { [$0] } ?? []))
    }

    static func readWindowSettings(_ json: Json) -> [String: AppWindowSetting] {
        var settings: [String: AppWindowSetting] = [:]
        for app in json["apps"].items {
            let package = app.string("package")
            if package.isEmpty {
                continue
            }
            let size = app["start_size"].items
            var width: Int?
            var height: Int?
            if size.count == 2, let w = size[0].number, let h = size[1].number {
                width = w.intValue
                height = h.intValue
            }
            settings[package] = AppWindowSetting(package: package, fixedSize: app.bool("fixed_size"),
                                                 startWidth: width, startHeight: height,
                                                 dpiScaling: DpiScaling.read(app["dpi_scaling"].string))
        }
        return settings
    }

    /// The user's window settings for every app that has any, by package.
    public func windowSettings() async throws -> [String: AppWindowSetting] {
        try await windowSettingsList().apps
    }

    /// The user's window settings for every app that has any, by package, and whether this
    /// build of Android scales an app the System (forced) way yet. A dsa that does not say
    /// counts as one whose Android cannot.
    public func windowSettingsList() async throws -> (apps: [String: AppWindowSetting], systemForcedAvailable: Bool) {
        let json = try await service.runJson(["app-window"])
        return (AppsService.readWindowSettings(json), json.bool("system_forced_available"))
    }

    /// Replaces the window settings of one app, and returns them as the service now has them.
    public func setWindowSetting(_ setting: AppWindowSetting) async throws -> AppWindowSetting {
        let json = try await service.runJson([
            "app-window", setting.package,
            "--fixed-size", setting.fixedSize ? "on" : "off",
            "--start-size", setting.startSizeArgument,
            "--dpi-scaling", setting.dpiScaling.rawValue,
        ])
        return AppsService.readWindowSettings(json)[setting.package] ?? .standard(setting.package)
    }

    /// Installs `package` again with its ARM code (`on`) or its x86_64 code, keeping its data
    /// unless `eraseData`. A refusal is a report with its reason; anything else that kept the
    /// request from Android is a `DsaError`.
    public func runAsArm(_ package: String, on: Bool, eraseData: Bool) async throws -> RunAsArmReport {
        var args = ["apps", "run-as-arm", package, on ? "on" : "off"]
        if eraseData {
            args.append("--erase-data")
        }
        let result = try await service.cli.run(args + ["--json"])
        if result.exitCode != 0 {
            if let refused = DsaService.refusal(DsaError.detail(of: result), verb: "changed") {
                return RunAsArmReport(done: false, status: refused.reason, message: refused.detail, on: !on)
            }
            throw DsaError.failure(result)
        }
        let json = try Json.parse(result.output)
        return RunAsArmReport(done: json.string("status") == "done", status: json.string("status"),
                              message: json.string("message"), on: json.bool("on"))
    }

    /// Sets how far `package` may use root. A refusal by the service (root elevation off for
    /// the Mac, or `remember` without an administrator's approval) is a report with its words;
    /// anything else is a `DsaError`.
    public func setRoot(_ package: String, _ level: RootLevel) async throws -> RootReport {
        let result = try await service.cli.run(["apps", "root", package, level.rawValue, "--json"])
        if result.exitCode != 0 {
            let detail = DsaError.detail(of: result)
            let prefix = "not changed: \(package): "
            if detail.hasPrefix(prefix) {
                return RootReport(done: false, level: "", message: String(detail.dropFirst(prefix.count)))
            }
            throw DsaError.failure(result)
        }
        let json = try Json.parse(result.output)
        return RootReport(done: true, level: json.string("level"), message: "")
    }

    /// Uninstalls `package`. A refusal by Android is a report with its reason; anything else
    /// that kept the request from Android is a `DsaError`.
    public func uninstall(_ package: String) async throws -> UninstallReport {
        let result = try await service.cli.run(["apps", "uninstall", package, "--json"])
        if result.exitCode != 0 {
            if let refused = DsaService.refusal(DsaError.detail(of: result), verb: "uninstalled") {
                return UninstallReport(uninstalled: false, status: refused.reason, message: refused.detail)
            }
            throw DsaError.failure(result)
        }
        let json = try Json.parse(result.output)
        return UninstallReport(uninstalled: json.bool("uninstalled"), status: json.string("status"), message: json.string("message"))
    }
}

/// One app on the Apps page.
public struct AppRow: Equatable, Identifiable, Sendable {
    public var package, label, detail, size: String
    public var icon: Data?
    public var canUninstall: Bool
    public var isSystem: Bool
    public var uninstallTip: String?
    public var resizable: Bool
    /// Where the app's start size is among the choices; -1 for a size set another way.
    public var startSizeIndex: Int
    /// Where the app's scaling choice is among `dpiScalingChoices`, and, while it is System
    /// (forced) and not available yet, why it works like System, in the user's language.
    public var dpiScalingIndex = 0
    public var dpiScalingTip: String?
    /// Whether the app runs its ARM code through the translator, whether that can be changed,
    /// and if not, why, in the user's language.
    public var runAsArm = false
    public var canRunAsArm = false
    public var runAsArmTip: String?
    /// Where the app's root level is among `rootChoices`; -1 when it cannot be chosen, and
    /// then why, in the user's language.
    public var rootIndex = -1
    public var canChangeRoot = false
    public var rootTip: String?
    /// For an app that shares its user ID with other apps: which apps, and that their root
    /// level is one setting, in the user's language; nil for an app with a user ID of its own.
    public var rootGroupNote: String?

    public var id: String { package }
}

/// Android refused; the message says why in the user's language.
private struct RefusedByAndroid: Error {
    var message: String
}

/// The Apps page: every app installed in Android, searchable, each to open, to see in
/// Android's own settings, to uninstall, or to change how its windows size; and Android's own
/// list of apps.
@MainActor
public final class AppsViewModel: ObservableObject {
    private let service: AppsService
    private let text: TextSource

    @Published public private(set) var apps: [InstalledAppInfo]?
    @Published public private(set) var windows: [String: AppWindowSetting] = [:]
    /// Whether this build of Android scales an app the System (forced) way yet.
    @Published public private(set) var systemForcedAvailable = false
    /// What the user typed to find an app: matched against its name and package.
    @Published public var search = ""
    @Published public private(set) var error: String?
    @Published public private(set) var message: String?
    @Published public private(set) var isBusy = false

    public init(service: AppsService, text: TextSource) {
        self.service = service
        self.text = text
    }

    public var hasNoApps: Bool { apps?.isEmpty == true }
    public var hasNoMatches: Bool { (apps?.isEmpty == false) && rows.isEmpty }

    /// How many apps are installed, and how many the search shows.
    public var count: String {
        guard let apps else {
            return ""
        }
        return search.trimmingCharacters(in: .whitespaces).isEmpty
            ? text.format("Apps_Count", apps.count)
            : text.format("Apps_CountMatching", rows.count, apps.count)
    }

    /// A date in milliseconds since the Unix epoch, in the user's short date format.
    public nonisolated static func date(_ ms: UInt64, locale: Locale = .current, timeZone: TimeZone = .current) -> String {
        let formatter = DateFormatter()
        formatter.locale = locale
        formatter.timeZone = timeZone
        formatter.dateStyle = .short
        formatter.timeStyle = .none
        return formatter.string(from: Date(timeIntervalSince1970: Double(min(ms, UInt64(Int64.max))) / 1000))
    }

    /// A size in bytes, as the Memory page shows sizes.
    public nonisolated static func size(_ bytes: UInt64, locale: Locale = .current) -> String {
        if bytes > 0 && bytes < 1024 * 1024 {
            return "\(Int((Double(bytes) / 1024).rounded(.up))) KB"
        }
        return Format.size(bytes / 1024, locale: locale)
    }

    /// The apps the search matches, by name.
    public var rows: [AppRow] {
        let query = search.trimmingCharacters(in: .whitespaces)
        func found(_ app: InstalledAppInfo) -> Bool {
            query.isEmpty || app.label.localizedCaseInsensitiveContains(query)
                || app.package.range(of: query, options: .caseInsensitive) != nil
        }
        // Apps that share a user ID are one group: listed together, in the place of the
        // group's first app by name, and all shown when a search finds one of them.
        func groupName(_ app: InstalledAppInfo) -> String {
            ([app] + sharedWith(app)).map(AppsViewModel.name)
                .min { $0.localizedCaseInsensitiveCompare($1) == .orderedAscending } ?? AppsViewModel.name(app)
        }
        return (apps ?? [])
            .filter { found($0) || sharedWith($0).contains(where: found) }
            .sorted { a, b in
                let order = groupName(a).localizedCaseInsensitiveCompare(groupName(b))
                if order != .orderedSame { return order == .orderedAscending }
                if a.sharedUserId != b.sharedUserId { return a.sharedUserId < b.sharedUserId }
                return AppsViewModel.name(a).localizedCaseInsensitiveCompare(AppsViewModel.name(b)) == .orderedAscending
            }
            .map(row)
    }

    /// The name the page shows for `app`: its label, or its package when it has none.
    private static func name(_ app: InstalledAppInfo) -> String { app.label.isEmpty ? app.package : app.label }

    /// The other apps that share `app`'s user ID, by name.
    private func sharedWith(_ app: InstalledAppInfo) -> [InstalledAppInfo] {
        guard !app.sharedUserId.isEmpty else { return [] }
        return (apps ?? [])
            .filter { $0.sharedUserId == app.sharedUserId && $0.package != app.package }
            .sorted { AppsViewModel.name($0).localizedCaseInsensitiveCompare(AppsViewModel.name($1)) == .orderedAscending }
    }

    /// What the page says under an app's root level when it shares its user ID with other
    /// apps; nil for an app with a user ID of its own.
    public func rootGroupNote(_ app: InstalledAppInfo) -> String? {
        let others = sharedWith(app).map(AppsViewModel.name)
        return others.isEmpty ? nil : text.format("Apps_RootGroup", others.joined(separator: ", "))
    }

    /// The start sizes the page offers: Android's own, then phone and tablet sizes, in
    /// density-independent pixels.
    public var startSizeChoices: [StartSizeChoice] {
        [
            StartSizeChoice(label: text.get("Apps_StartSize_Auto"), width: nil, height: nil),
            choice("Apps_StartSize_PhonePortrait", 412, 915),
            choice("Apps_StartSize_PhoneLandscape", 915, 412),
            choice("Apps_StartSize_TabletPortrait", 800, 1280),
            choice("Apps_StartSize_TabletLandscape", 1280, 800),
        ]
    }

    private func choice(_ key: String, _ width: Int, _ height: Int) -> StartSizeChoice {
        StartSizeChoice(label: text.format(key, width, height), width: width, height: height)
    }

    /// The ways an app's window can follow a monitor of another scale, in the order of
    /// `DpiScaling`. System (forced) is stored and can be chosen, but until Android can do it,
    /// the list marks it as not available yet.
    public var dpiScalingChoices: [DpiScalingChoice] {
        DpiScaling.allCases.map { scaling in
            if scaling == .systemForced && !systemForcedAvailable {
                return DpiScalingChoice(label: text.get("Apps_DpiScaling_SystemForcedNotYet"), scaling: scaling,
                                        available: false)
            }
            return DpiScalingChoice(label: text.get(scaling.textKey), scaling: scaling, available: true)
        }
    }

    /// Why System (forced) works like System for now, while it is the app's choice and not
    /// available; otherwise nil.
    public func dpiScalingTip(_ setting: AppWindowSetting) -> String? {
        setting.dpiScaling == .systemForced && !systemForcedAvailable
            ? text.get("Apps_DpiScaling_SystemForcedTip") : nil
    }

    /// The window settings of `package`.
    public func windowOf(_ package: String) -> AppWindowSetting { windows[package] ?? .standard(package) }

    private func startSizeIndex(_ setting: AppWindowSetting) -> Int {
        startSizeChoices.firstIndex { $0.width == setting.startWidth && $0.height == setting.startHeight } ?? -1
    }

    private func row(_ app: InstalledAppInfo) -> AppRow {
        var parts = [app.package]
        if !app.versionName.isEmpty {
            parts.append(text.format("Apps_Version", app.versionName))
        }
        if app.firstInstallMs > 0 {
            parts.append(text.format("Apps_Installed", AppsViewModel.date(app.firstInstallMs)))
        }
        if app.systemApp {
            parts.append(text.get("Apps_System"))
        }
        let window = windowOf(app.package)
        return AppRow(
            package: app.package, label: app.label.isEmpty ? app.package : app.label,
            detail: parts.joined(separator: " · "), size: AppsViewModel.size(app.totalBytes),
            icon: app.iconPng.isEmpty ? nil : app.iconPng, canUninstall: !app.systemApp, isSystem: app.systemApp,
            uninstallTip: app.systemApp ? text.get("Apps_SystemTip") : nil,
            resizable: !window.fixedSize, startSizeIndex: startSizeIndex(window),
            dpiScalingIndex: DpiScaling.allCases.firstIndex(of: window.dpiScaling) ?? 0,
            dpiScalingTip: dpiScalingTip(window),
            runAsArm: app.runAsArm.on, canRunAsArm: app.runAsArm.available,
            runAsArmTip: runAsArmTip(app.runAsArm),
            rootIndex: RootLevel.read(app.root)?.index ?? -1, canChangeRoot: RootLevel.read(app.root) != nil,
            rootTip: rootTip(app.root), rootGroupNote: rootGroupNote(app))
    }

    /// Why the "Run as ARM" switch cannot be changed, in the user's language; nil when it can.
    public func runAsArmTip(_ arm: RunAsArmSwitch) -> String? {
        arm.available ? nil : text.get("Apps_RunAsArm_\(arm.reason.isEmpty ? "unknown" : arm.reason)")
    }

    /// The root levels the page offers, in the order of `RootLevel`.
    public var rootChoices: [RootChoice] {
        RootLevel.allCases.map { RootChoice(label: text.get("Apps_Root_\($0.rawValue)"), level: $0) }
    }

    /// Whether root elevation is off for the Mac, so no app's level can be chosen and the page
    /// says where to turn it on.
    public var rootUnavailable: Bool { apps?.contains { $0.root == "unavailable" } ?? false }

    /// Why an app's root level cannot be chosen, in the user's language; nil when it can.
    public func rootTip(_ root: String) -> String? {
        RootLevel.read(root) != nil ? nil : text.get(root == "unavailable" ? "Apps_RootTip_unavailable" : "Apps_RootTip_unknown")
    }

    /// Sets how far the app may use root: the level at `index` of `rootChoices`. `remember`
    /// needs an administrator's approval, which the service asks for; a refusal is the page's
    /// error with the service's words.
    public func setRoot(_ package: String, _ index: Int) async {
        guard RootLevel.allCases.indices.contains(index) else {
            return
        }
        let level = RootLevel.allCases[index]
        if let current = apps?.first(where: { $0.package == package }), RootLevel.read(current.root) == level {
            return
        }
        await run(reload: true) {
            let label = labelOf(package)
            let report = try await service.setRoot(package, level)
            if !report.done {
                throw RefusedByAndroid(message: text.format("Apps_RootNotChanged", label, report.message))
            }
            let now = RootLevel.read(report.level) ?? level
            return text.format("Apps_RootSet_\(now.rawValue)", label)
        }
    }

    /// The name an app goes by on the page.
    public func labelOf(_ package: String) -> String {
        if let app = apps?.first(where: { $0.package == package }), !app.label.isEmpty {
            return app.label
        }
        return package
    }

    private func run(reload: Bool, _ work: () async throws -> String?) async {
        if isBusy {
            return
        }
        isBusy = true
        message = nil
        do {
            message = try await work()
            if reload {
                apps = try await service.list()
            }
            error = nil
        } catch let refused as RefusedByAndroid {
            error = refused.message
        } catch {
            self.error = Format.problem(text, error)
        }
        isBusy = false
    }

    /// Reads the installed apps.
    public func refresh() async { await run(reload: true) { nil } }

    /// Reads the user's window settings for the apps. A service that keeps none leaves every
    /// app at the defaults.
    public func loadWindowSettings() async {
        await run(reload: false) {
            do {
                let list = try await service.windowSettingsList()
                windows = list.apps
                systemForcedAvailable = list.systemForcedAvailable
            } catch let failed as DsaError where failed.problem == .refused {
                windows = [:]
                systemForcedAvailable = false
            }
            return nil
        }
    }

    private func setWindow(_ wanted: AppWindowSetting) async {
        await run(reload: false) {
            let now = try await service.setWindowSetting(wanted)
            if now == .standard(now.package) {
                windows.removeValue(forKey: now.package)
            } else {
                windows[now.package] = now
            }
            return text.format("Apps_WindowChanged", labelOf(now.package))
        }
    }

    /// Lets the user resize the app's windows, or keeps them at the size they open at.
    public func setResizable(_ package: String, _ resizable: Bool) async {
        var wanted = windowOf(package)
        if wanted.fixedSize == !resizable {
            return
        }
        wanted.fixedSize = !resizable
        await setWindow(wanted)
    }

    /// Opens the app's windows at the start size at `index` of `startSizeChoices`.
    public func setStartSize(_ package: String, _ index: Int) async {
        let choices = startSizeChoices
        guard choices.indices.contains(index) else {
            return
        }
        var wanted = windowOf(package)
        if wanted.startWidth == choices[index].width && wanted.startHeight == choices[index].height {
            return
        }
        wanted.startWidth = choices[index].width
        wanted.startHeight = choices[index].height
        await setWindow(wanted)
    }

    /// Has the app's windows follow a monitor of another scale the way at `index` of
    /// `dpiScalingChoices`. Windows that move from now on follow it.
    public func setDpiScaling(_ package: String, _ index: Int) async {
        let all = DpiScaling.allCases
        guard all.indices.contains(index) else {
            return
        }
        var wanted = windowOf(package)
        if wanted.dpiScaling == all[index] {
            return
        }
        wanted.dpiScaling = all[index]
        await setWindow(wanted)
    }

    /// Opens the app in a window of its own.
    public func open(_ package: String) async {
        await run(reload: false) {
            try await service.open(package)
            return nil
        }
    }

    /// Opens Android's own page of the app: its permissions, storage and notifications.
    public func info(_ package: String) async {
        await run(reload: false) {
            try await service.openSettings(package)
            return nil
        }
    }

    /// Puts a shortcut to the app on the Desktop, and says so.
    public func desktopShortcut(_ package: String) async {
        await run(reload: false) {
            try await service.desktopShortcut(package)
            return text.format("Apps_DesktopShortcutMade", labelOf(package))
        }
    }

    /// Opens Android's own list of every app.
    public func manage() async {
        await run(reload: false) {
            try await service.openSettings(nil)
            return nil
        }
    }

    /// Installs the app again with its ARM code (`on`) or its x86_64 code. The app keeps its
    /// data; when Android cannot keep it, `confirmErase` asks the user (with the app's name)
    /// whether to go on and erase it. A refusal is the page's error, in the user's language.
    public func setRunAsArm(_ package: String, _ on: Bool, confirmErase: (String) async -> Bool) async {
        await run(reload: true) {
            let label = labelOf(package)
            var report = try await service.runAsArm(package, on: on, eraseData: false)
            if !report.done && report.status == "data-would-be-erased" {
                let erase = await confirmErase(label)
                if !erase {
                    return text.format("Apps_RunAsArmKept", label)
                }
                report = try await service.runAsArm(package, on: on, eraseData: true)
            }
            if !report.done {
                let reason = text.get("Apps_RunAsArmRefused_\(report.status)")
                throw RefusedByAndroid(message: text.format("Apps_RunAsArmNotChanged", label, reason, report.message))
            }
            return text.format(report.on ? "Apps_RunAsArmOn" : "Apps_RunAsArmOff", label)
        }
    }

    /// Uninstalls the app; the page asks the user first. A refusal is the page's error, in the
    /// user's language.
    public func uninstall(_ package: String) async {
        await run(reload: true) {
            let label = labelOf(package)
            let report = try await service.uninstall(package)
            if !report.uninstalled {
                let reason = text.get("Apps_Refused_\(report.status)")
                throw RefusedByAndroid(message: text.format("Apps_NotUninstalled", label, reason, report.message))
            }
            return text.format("Apps_Uninstalled", label)
        }
    }
}
