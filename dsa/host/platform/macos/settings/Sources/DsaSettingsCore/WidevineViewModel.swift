import Foundation

/// The Widevine module installed for the user, as `dsa drm widevine` reports it.
public struct WidevinePackageInfo: Equatable, Sendable {
    /// The APEX's file name, com.google.android.widevine.apex.
    public var apex: String
    /// Google's image it came from: its build, API level and revision.
    public var version: String
    public var androidMajor: Int
    public var abi: String
    public var source: String
}

/// Where Widevine stands for the user.
public struct WidevineInfo: Equatable, Sendable {
    public var package: WidevinePackageInfo?
    public var onDisk: Bool
    public var running: Bool
    public var active: Bool
    public var restartNeeded: Bool
    public var securityLevel: String

    /// Google's terms for what its SDK repository gives: the Android SDK License.
    public static let termsUrl = "https://developer.android.com/studio/terms"
    /// Where the emulator image Widevine is taken from is downloaded.
    public static let source = "https://dl.google.com/android/repository/sys-img/google_apis/"
}

public extension DsaService {
    /// Reads what `dsa drm widevine` printed. A field a service does not report reads as empty.
    static func readWidevine(_ json: Json) -> WidevineInfo {
        let p = json["package"]
        let level = json.string("security_level")
        return WidevineInfo(
            package: p.isObject
                ? WidevinePackageInfo(
                    apex: p.string("apex"), version: p.string("version"), androidMajor: p.int("android_major"),
                    abi: p.string("abi"), source: p.string("source"))
                : nil,
            onDisk: json.bool("on_disk"), running: json.bool("running"), active: json.bool("active"),
            restartNeeded: json.bool("restart_needed"), securityLevel: level.isEmpty ? "L3" : level)
    }

    func widevine() async throws -> WidevineInfo {
        DsaService.readWidevine(try await runJson(["drm", "widevine", "status"]))
    }

    /// Downloads Google's emulator image for this Android, takes Widevine out of it unchanged and
    /// installs it. The user agreed to Google's terms in the app first, so dsa does not ask again.
    /// It takes minutes; Android uses the module from its next start.
    func installWidevine() async throws -> WidevineInfo {
        DsaService.readWidevine(try await runJson(["drm", "widevine", "install", "--yes"]))
    }

    /// Removes Widevine and the device certificates it kept, from Android's next start.
    func removeWidevine() async throws -> WidevineInfo {
        DsaService.readWidevine(try await runJson(["drm", "widevine", "remove"]))
    }
}

/// The protected content item on the Advanced page: whether Widevine is installed, taking it from
/// Google's image with the user's agreement, removing it, and what L3 means for streaming.
@MainActor
public final class WidevineViewModel: ObservableObject {
    /// What the item says, as the page shows it.
    public enum State: Equatable, Sendable {
        case notInstalled
        case installed
        /// What is installed differs from what Android started with.
        case restartPending
    }

    private let service: DsaService
    private let text: TextSource

    @Published public private(set) var info: WidevineInfo?
    @Published public private(set) var error: String?
    @Published public private(set) var isBusy = false
    @Published public private(set) var isInstalling = false

    public init(service: DsaService, text: TextSource) {
        self.service = service
        self.text = text
    }

    public var loaded: Bool { info != nil }
    public var canChange: Bool { loaded && !isBusy }
    public var hasPackage: Bool { info?.package != nil }
    public var canInstall: Bool { canChange && !hasPackage }
    public var canRemove: Bool { canChange && hasPackage }
    public var restartNeeded: Bool { info?.restartNeeded ?? false }

    public var state: State {
        if restartNeeded {
            return .restartPending
        }
        return hasPackage ? .installed : .notInstalled
    }

    /// One line on the install state.
    public var statusText: String {
        guard let info else {
            return text.get("Widevine_Unknown")
        }
        switch state {
        case .notInstalled:
            return text.get("Widevine_NotInstalled")
        case .installed:
            return installedText(info, key: "Widevine_Installed")
        case .restartPending:
            return info.package == nil
                ? text.get("Widevine_RemovedRestart")
                : installedText(info, key: "Widevine_InstalledRestart")
        }
    }

    private func installedText(_ info: WidevineInfo, key: String) -> String {
        guard let package = info.package else {
            return ""
        }
        return text.format(key, package.version, package.androidMajor > 0 ? String(package.androidMajor) : "?",
                           package.abi.isEmpty ? "?" : package.abi)
    }

    /// What the confirmation says before anything is downloaded: where it comes from, and the
    /// terms the user accepts.
    public var notice: String { text.format("Widevine_Notice", WidevineInfo.source, WidevineInfo.termsUrl) }

    /// That streaming services send an L3 device standard definition at most, and where to watch
    /// in high definition.
    public var qualityText: String { text.get("Widevine_Quality") }

    public var termsUrl: String { WidevineInfo.termsUrl }

    private func act(installing: Bool, _ work: () async throws -> WidevineInfo) async {
        isBusy = true
        isInstalling = installing
        error = nil
        do {
            info = try await work()
        } catch {
            self.error = Format.problem(text, error)
        }
        isBusy = false
        isInstalling = false
    }

    /// Reads where Widevine stands. A service that cannot say leaves the item unknown, with its
    /// buttons off.
    public func load() async {
        info = try? await service.widevine()
    }

    /// Takes Widevine from Google's image and installs it; the user has agreed in the confirmation.
    public func install() async {
        if !canInstall { return }
        await act(installing: true) { try await service.installWidevine() }
    }

    public func remove() async {
        if !canRemove { return }
        await act(installing: false) { try await service.removeWidevine() }
    }
}
