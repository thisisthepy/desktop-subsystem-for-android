import Foundation

/// The Google apps package installed for the user, as `dsa gapps` reports it.
public struct GappsPackageInfo: Equatable, Sendable {
    public var variant: String
    /// The package's build date, such as 20261001.
    public var version: String
    /// The Android version the package is for, such as 16.0.
    public var androidVersion: String
    public var sdk: Int
    public var arch: String
    /// The file that was downloaded, and where from.
    public var file: String
    public var source: String
}

/// What the running Android says of the Google apps.
public struct GappsGuestInfo: Equatable, Sendable {
    /// The package's image is on the Android disk.
    public var installed: Bool
    /// This boot runs with the Google apps laid over the system.
    public var active: Bool
    public var sdk: Int
    public var abi: String
    public var abilist: String
    /// The Google Services Framework's Android ID, which Google's registration page asks for;
    /// empty until it is known.
    public var deviceId: String
}

/// Where Google Play stands for the user.
public struct GappsInfo: Equatable, Sendable {
    public var package: GappsPackageInfo?
    public var android: GappsGuestInfo?
    public var restartNeeded: Bool
    public var termsUrl: String
    public var registrationUrl: String

    public static let defaultTermsUrl = "https://policies.google.com/terms"
    public static let defaultRegistrationUrl = "https://www.google.com/android/uncertified/"
}

public extension DsaService {
    /// Reads what `dsa gapps` printed. A field a service does not report reads as empty.
    static func readGapps(_ json: Json) -> GappsInfo {
        let p = json["package"]
        let a = json["android"]
        let terms = json.string("terms_url")
        let registration = json.string("registration_url")
        return GappsInfo(
            package: p.isObject
                ? GappsPackageInfo(
                    variant: p.string("variant"), version: p.string("version"),
                    androidVersion: p.string("android_version"), sdk: p.int("sdk"), arch: p.string("arch"),
                    file: p.string("file"), source: p.string("source"))
                : nil,
            android: a.isObject
                ? GappsGuestInfo(
                    installed: a.bool("installed"), active: a.bool("active"), sdk: a.int("sdk"),
                    abi: a.string("abi"), abilist: a.string("abilist"), deviceId: a.string("device_id"))
                : nil,
            restartNeeded: json.bool("restart_needed"),
            termsUrl: terms.isEmpty ? GappsInfo.defaultTermsUrl : terms,
            registrationUrl: registration.isEmpty ? GappsInfo.defaultRegistrationUrl : registration)
    }

    func gapps() async throws -> GappsInfo {
        DsaService.readGapps(try await runJson(["gapps", "status"]))
    }

    /// Downloads the Google apps package for the user's Android, makes its image and sends it
    /// there. It takes minutes; Android is started first when it is not running.
    func installGapps() async throws -> GappsInfo {
        DsaService.readGapps(try await runJson(["gapps", "install"]))
    }

    /// Removes the Google apps; they are gone from the next start of Android.
    func removeGapps() async throws -> GappsInfo {
        DsaService.readGapps(try await runJson(["gapps", "remove"]))
    }
}

/// The Google Play item on the Advanced page: whether Google's apps are installed, installing
/// and removing them, and how to register this device with Google.
@MainActor
public final class GappsViewModel: ObservableObject {
    /// What the item says, as the page shows it.
    public enum State: Equatable, Sendable {
        /// No package is installed and Android runs without one.
        case notInstalled
        /// A package is installed and Android runs with it, or will at its next start.
        case installed
        /// What is installed differs from what Android started with.
        case restartPending
    }

    private let service: DsaService
    private let text: TextSource

    @Published public private(set) var info: GappsInfo?
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
            return text.get("Gapps_Unknown")
        }
        switch state {
        case .notInstalled:
            return text.get("Gapps_NotInstalled")
        case .installed:
            return installedText(info)
        case .restartPending:
            return info.package == nil ? text.get("Gapps_RemovedRestart") : text.get("Gapps_InstalledRestart")
        }
    }

    /// The package's version and where it came from; empty without one.
    public var packageText: String {
        guard let info, info.package != nil else {
            return ""
        }
        return installedText(info)
    }

    private func installedText(_ info: GappsInfo) -> String {
        guard let package = info.package else {
            return ""
        }
        let version = package.androidVersion.isEmpty ? "?" : package.androidVersion
        let arch = package.arch.isEmpty ? "?" : package.arch
        let file = package.file.isEmpty ? package.source : package.file
        return text.format("Gapps_Installed", package.version, version, arch, file)
    }

    public var termsUrl: String { info?.termsUrl ?? GappsInfo.defaultTermsUrl }
    public var registrationUrl: String { info?.registrationUrl ?? GappsInfo.defaultRegistrationUrl }

    /// The ID Google's registration page asks for; empty until Android reports it.
    public var deviceId: String { info?.android?.deviceId ?? "" }
    public var hasDeviceId: Bool { !deviceId.isEmpty }

    private func act(installing: Bool, _ work: () async throws -> GappsInfo) async {
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

    /// Reads where Google Play stands. A service that cannot say leaves the item unknown, with
    /// its buttons off.
    public func load() async {
        info = try? await service.gapps()
    }

    public func install() async {
        if !canInstall { return }
        await act(installing: true) { try await service.installGapps() }
    }

    public func remove() async {
        if !canRemove { return }
        await act(installing: false) { try await service.removeGapps() }
    }
}
