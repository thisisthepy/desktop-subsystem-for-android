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

/// One app installed in Android, as `dsa apps list` reports it.
public struct InstalledAppInfo: Equatable, Sendable {
    public var package, label, versionName: String
    public var versionCode: Int64
    public var firstInstallMs, lastUpdateMs: UInt64
    public var systemApp: Bool
    public var appBytes, dataBytes, cacheBytes: UInt64
    public var iconPng: Data
    public var runAsArm: RunAsArmSwitch = .unknown

    public var totalBytes: UInt64 { appBytes + dataBytes + cacheBytes }
}

/// The user's window settings for one app, as `dsa app-window` reports them: whether its
/// windows keep their size, and the size they open at in density-independent pixels (nil for
/// the size Android gives the task).
public struct AppWindowSetting: Equatable, Sendable {
    public var package: String
    public var fixedSize: Bool
    public var startWidth: Int?
    public var startHeight: Int?

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
            iconPng: app.base64("icon_png"), runAsArm: readRunAsArm(app))
    }

    /// Every app installed in Android, with its size read again; Android is started first when
    /// it is not running.
    public func list() async throws -> [InstalledAppInfo] {
        try await service.runJson(["apps", "list"])["apps"].items.map(AppsService.readApp).filter { !$0.package.isEmpty }
    }

    /// Opens the app in a window of its own.
    public func open(_ package: String) async throws { _ = try await service.runText(["launch", package]) }

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
                                                 startWidth: width, startHeight: height)
        }
        return settings
    }

    /// The user's window settings for every app that has any, by package.
    public func windowSettings() async throws -> [String: AppWindowSetting] {
        AppsService.readWindowSettings(try await service.runJson(["app-window"]))
    }

    /// Replaces the window settings of one app, and returns them as the service now has them.
    public func setWindowSetting(_ setting: AppWindowSetting) async throws -> AppWindowSetting {
        let json = try await service.runJson([
            "app-window", setting.package,
            "--fixed-size", setting.fixedSize ? "on" : "off",
            "--start-size", setting.startSizeArgument,
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
    /// Whether the app runs its ARM code through the translator, whether that can be changed,
    /// and if not, why, in the user's language.
    public var runAsArm = false
    public var canRunAsArm = false
    public var runAsArmTip: String?

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
        return (apps ?? [])
            .filter {
                query.isEmpty || $0.label.localizedCaseInsensitiveContains(query)
                    || $0.package.range(of: query, options: .caseInsensitive) != nil
            }
            .sorted {
                ($0.label.isEmpty ? $0.package : $0.label)
                    .localizedCaseInsensitiveCompare($1.label.isEmpty ? $1.package : $1.label) == .orderedAscending
            }
            .map(row)
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
            runAsArm: app.runAsArm.on, canRunAsArm: app.runAsArm.available,
            runAsArmTip: runAsArmTip(app.runAsArm))
    }

    /// Why the "Run as ARM" switch cannot be changed, in the user's language; nil when it can.
    public func runAsArmTip(_ arm: RunAsArmSwitch) -> String? {
        arm.available ? nil : text.get("Apps_RunAsArm_\(arm.reason.isEmpty ? "unknown" : arm.reason)")
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
                windows = try await service.windowSettings()
            } catch let failed as DsaError where failed.problem == .refused {
                windows = [:]
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
