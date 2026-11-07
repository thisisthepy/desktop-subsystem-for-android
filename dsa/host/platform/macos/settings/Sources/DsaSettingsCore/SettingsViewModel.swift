import Foundation

/// One choice of render GPU: what the list shows and the value the setting keeps.
public struct GpuChoice: Equatable, Hashable, Sendable {
    public var label: String
    public var value: String
}

/// One choice of memory limit: what the list shows and the size in MiB (0 is automatic).
public struct MemoryLimitChoice: Equatable, Hashable, Sendable {
    public var label: String
    public var mb: Int
}

/// The settings the Memory, Display, Updates, Network, Advanced and About pages show and
/// change.
@MainActor
public final class SettingsViewModel: ObservableObject {
    /// The smallest limit the service takes, in MiB. Android does not start in less.
    public static let smallestMemoryLimitMb = 2048
    static let offeredLimitsMb = [2048, 3072, 4096, 6144, 8192, 12288, 16384]

    private let service: DsaService
    private let text: TextSource

    @Published public private(set) var settings: SettingsInfo?
    @Published public private(set) var arm: ArmTranslationInfo?
    @Published public private(set) var gpus: [GpuInfo] = []
    @Published public private(set) var error: String?
    @Published public private(set) var isBusy = false
    @Published public private(set) var isResetting = false
    /// Whether Google's ARM translator is being downloaded and installed now.
    @Published public private(set) var isFetchingGoogleArm = false
    /// Whether the last reset finished, so the page can say the data is gone.
    @Published public private(set) var resetDone = false
    /// The network as last read from `dsa network`; nil until a refresh, then the settings'.
    @Published private var readNetwork: NetworkInfo?
    /// Whether the app runs as an administrator (root). The settings that apply to every
    /// user are taken only from one; the pages offer to change them as an administrator.
    @Published public var isAdministrator = false

    public init(service: DsaService, text: TextSource) {
        self.service = service
        self.text = text
    }

    public var loaded: Bool { settings != nil }
    public var hasError: Bool { error != nil }
    public var canChange: Bool { loaded && !isBusy }

    public var freeze: Bool { settings?.freeze ?? false }
    /// Whether web links apps in Android open go to Android apps instead of the default browser.
    public var linksInAndroid: Bool { settings?.linksInAndroid ?? false }
    /// Whether text copied on the Mac can be pasted in Android, and the reverse.
    public var clipboard: Bool { settings?.clipboard ?? true }
    /// Whether the chosen folders show in Android's shared storage.
    public var fileSharing: Bool { settings?.fileSharing ?? true }
    /// Whether the folder choices can be changed: only while sharing is on.
    public var canChooseFolders: Bool { canChange && fileSharing }
    public func isFolderShared(_ folder: String) -> Bool {
        (settings?.folders ?? SettingsInfo.allFolders).contains(folder)
    }

    public var developerMode: Bool { settings?.developerMode ?? false }
    public var adbPort: Int { settings?.adbPort ?? SettingsInfo.defaultAdbPort }
    public var adbConnect: String { settings?.adbConnect ?? "adb connect 127.0.0.1:\(SettingsInfo.defaultAdbPort)" }
    /// Whether developer mode's port is attached to this Mac's adb server by itself, so adb and
    /// Android Studio list Android without adb connect.
    public var adbAutoConnect: Bool { settings?.adbAutoConnect ?? true }
    /// Where auto-connect stands, as the service last reported it.
    public var adbAutoConnectState: AdbAutoConnectState { settings?.adbAutoConnectState ?? .off }
    /// Where auto-connect stands, in the user's language.
    public var adbAutoConnectStateText: String { text.get(adbAutoConnectState.textKey) }

    // MARK: ARM translation

    public var armTranslation: Bool { settings?.armTranslation ?? true }
    public var armSupported: Bool { arm?.supported ?? false }
    public var hasArmPackage: Bool { arm?.package != nil }
    public var canRemoveArmTranslation: Bool { canChange && hasArmPackage }
    public var armRestartNeeded: Bool { arm?.restartNeeded ?? false }
    /// Whether the image carries a default translator. Only an x86_64 image does; an arm64
    /// one, which every Apple silicon Mac runs, runs ARM apps itself.
    public var hasArmDefault: Bool { arm?.defaultPackage != nil }
    /// The remove row shows only where there is no default translator: where there is one,
    /// removing the package is going back to it, which its own row does.
    public var showsArmRemove: Bool { !hasArmDefault }
    /// Whether the default translator can be chosen again: a package is installed in its place.
    public var canUseDefaultArmTranslator: Bool { canChange && hasArmDefault && hasArmPackage }
    /// What the default translator is, and what going back to it does.
    public var armDefaultDescription: String {
        guard let d = arm?.defaultPackage else { return "" }
        return text.format("Arm_DefaultDescription", d.name, d.version, d.license.isEmpty ? text.get("Arm_NoLicense") : d.license)
    }

    /// One line on whether ARM apps run, and with what.
    public var armStatus: String {
        guard let arm else {
            return text.get("Arm_NotSupported")
        }
        if let translator = arm.translator {
            let license = translator.license.isEmpty ? text.get("Arm_NoLicense") : translator.license
            if translator.source == .builtIn {
                return text.format("Arm_SupportedDefault", translator.name, translator.version, license)
            }
            return text.format("Arm_Supported", translator.name, translator.version, license)
        }
        if !arm.enabled, let package = arm.package {
            return text.format("Arm_TurnedOff", package.name, package.version)
        }
        if !arm.enabled, let builtIn = arm.defaultPackage {
            return text.format("Arm_DefaultTurnedOff", builtIn.name, builtIn.version)
        }
        return text.get("Arm_NotSupported")
    }

    /// The ABIs the running Android installs apps for, or that it is not running.
    public var armAbis: String {
        guard let android = arm?.android else {
            return text.get("Arm_NotRunning")
        }
        return text.format("Arm_Abis", android.abilist.isEmpty ? "?" : android.abilist)
    }

    /// Whether the section offers Google's ARM translator. On an ARM64 Mac Android runs ARM
    /// apps itself, so dsa says it is not offered, and a service that cannot say offers nothing.
    public var showsGoogleArm: Bool { arm?.googleFetchAvailable ?? false }
    public var canFetchGoogleArm: Bool { canChange && showsGoogleArm }
    public var googleArmTermsUrl: String { DsaService.googleArmTermsUrl }

    /// What the confirmation says before anything is downloaded: where it comes from, and the
    /// terms the user accepts.
    public var googleArmNotice: String {
        text.format("ArmGoogle_Notice", DsaService.googleArmSource, DsaService.googleArmTermsUrl)
    }

    // MARK: Updates

    public var programUpdates: Bool { settings?.programUpdates ?? false }
    public var imageUpdates: Bool { settings?.imageUpdates ?? false }
    public var programUpdatesAvailable: Bool { settings?.programUpdatesAvailable ?? false }
    public var imageUpdatesAvailable: Bool { settings?.imageUpdatesAvailable ?? false }
    public var canChangeProgramUpdates: Bool { canChange && programUpdatesAvailable && isAdministrator }
    public var canChangeImageUpdates: Bool { canChange && imageUpdatesAvailable && isAdministrator }
    /// Whether a switch can be changed only by running dsa as an administrator.
    public var programUpdatesNeedAdministrator: Bool { canChange && programUpdatesAvailable && !isAdministrator }
    public var imageUpdatesNeedAdministrator: Bool { canChange && imageUpdatesAvailable && !isAdministrator }
    public var needsAdministrator: Bool { loaded && !isAdministrator && (programUpdatesAvailable || imageUpdatesAvailable) }
    /// Whether the newest version can be installed now; any user may ask.
    public var canUpdateNow: Bool { canChange && programUpdatesAvailable }

    // MARK: About

    public var serviceVersion: String { settings?.version ?? "" }
    public var androidVersion: String { settings?.androidVersion ?? "" }
    public var hasAndroidVersion: Bool { !(settings?.androidVersion ?? "").isEmpty }
    public var logDir: String { settings?.logDir ?? "" }
    public var hasLogDir: Bool { !(settings?.logDir ?? "").isEmpty }

    // MARK: Display

    public var renderGpu: String { settings?.renderGpu ?? "display" }

    /// The GPU that drives the main display, when it is known.
    public var displayGpuName: String { gpus.first(where: \.primaryDisplay)?.name ?? "" }

    /// The three kinds, then each GPU of this machine by name.
    public var gpuChoices: [GpuChoice] {
        var choices = [
            GpuChoice(label: text.get("Gpu_Display"), value: "display"),
            GpuChoice(label: text.get("Gpu_Integrated"), value: "integrated"),
            GpuChoice(label: text.get("Gpu_Discrete"), value: "discrete"),
        ]
        for gpu in gpus where !choices.contains(where: { $0.value == gpu.name }) {
            choices.append(GpuChoice(label: gpu.name, value: gpu.name))
        }
        // A name chosen before that matches no GPU found now still shows as chosen.
        if !choices.contains(where: { $0.value.caseInsensitiveCompare(renderGpu) == .orderedSame }) {
            choices.append(GpuChoice(label: renderGpu, value: renderGpu))
        }
        return choices
    }

    /// The choice the setting names, as the list spells it.
    public var selectedGpu: String {
        gpuChoices.first(where: { $0.value.caseInsensitiveCompare(renderGpu) == .orderedSame })?.value ?? "display"
    }

    // MARK: Memory limit and traces

    /// The most memory the user's Android may grow to, in MiB; 0 is automatic.
    public var memoryLimitMb: Int { settings?.memoryLimitMb ?? 0 }

    public var memoryLimitText: String {
        memoryLimitMb == 0 ? text.get("MemoryLimit_Auto") : SettingsViewModel.limitSize(memoryLimitMb)
    }

    static func limitSize(_ mb: Int) -> String {
        mb % 1024 == 0 ? "\(mb / 1024) GB" : "\(mb) MB"
    }

    /// Automatic, then the sizes on offer, then a size set elsewhere (with `dsa settings set
    /// memory-limit`) that is none of them.
    public var memoryLimitChoices: [MemoryLimitChoice] {
        var sizes = SettingsViewModel.offeredLimitsMb
        if memoryLimitMb != 0 && !sizes.contains(memoryLimitMb) {
            sizes.append(memoryLimitMb)
            sizes.sort()
        }
        return [MemoryLimitChoice(label: text.get("MemoryLimit_Auto"), mb: 0)]
            + sizes.map { MemoryLimitChoice(label: SettingsViewModel.limitSize($0), mb: $0) }
    }

    /// Whether every DSA process writes the input trace to its log.
    public var inputTrace: Bool { settings?.inputTrace ?? false }
    /// Whether every DSA process writes each stage of each resize to its log.
    public var resizeTrace: Bool { settings?.resizeTrace ?? false }
    /// The traces are the computer's settings, so only an administrator changes them.
    public var canChangeTraces: Bool { canChange && isAdministrator }
    public var tracesNeedAdministrator: Bool { canChange && !isAdministrator }

    // MARK: Network

    /// The network as last read: from `dsa network` after a refresh, else with the settings.
    public var network: NetworkInfo? { readNetwork ?? settings?.network }

    /// Whether Android joins this computer's network as a device of its own. Off by default.
    public var advancedNetworking: Bool { settings?.advancedNetworking ?? false }

    public var networkModeText: String {
        switch network?.mode ?? .unknown {
        case .nat: return text.get("Network_ModeNat")
        case .advanced: return text.get("Network_ModeAdvanced")
        case .off: return text.get("Network_ModeOff")
        case .unknown: return text.get("Network_ModeUnknown")
        }
    }

    /// Whether Android has a network whose addresses the page can show.
    public var hasNetworkDetails: Bool { network?.mode == .nat || network?.mode == .advanced }

    private func joinOrNone(_ items: [String]?) -> String {
        guard let items, !items.isEmpty else {
            return text.get("Network_None")
        }
        return items.joined(separator: ", ")
    }

    public var networkAddresses: String { joinOrNone(network?.addresses) }
    public var networkDns: String { joinOrNone(network?.dnsServers) }
    public var networkGateway: String {
        let gateway = network?.gateway ?? ""
        return gateway.isEmpty ? text.get("Network_None") : gateway
    }
    public var networkProblem: String { network?.problem ?? "" }
    public var hasNetworkProblem: Bool { !(network?.problem ?? "").isEmpty }

    /// Whether the switch differs from the network Android has now, which changes only when
    /// Android starts again. A problem explains a difference of its own.
    public var networkRestartNeeded: Bool {
        guard let network, network.mode == .nat || network.mode == .advanced, network.problem == nil else {
            return false
        }
        return advancedNetworking != (network.mode == .advanced)
    }

    // MARK: Data reset

    /// What the confirmation of a data reset says is lost, and what stays.
    public var resetWarning: String { text.get("ResetData_Warning") }
    public var canResetData: Bool { !isBusy && !isResetting }

    // MARK: Loading and changing

    private func change(_ work: () async throws -> SettingsInfo) async {
        isBusy = true
        error = nil
        do {
            settings = try await work()
        } catch {
            // The values stay as the service last reported them, so a switch moves back.
            self.error = Format.problem(text, error)
        }
        isBusy = false
    }

    /// Reads where ARM translation stands. A service that cannot say leaves the section saying
    /// ARM apps are not supported.
    private func loadArmTranslation() async {
        arm = try? await service.armTranslation()
    }

    public func load() async {
        await change { try await service.settings() }
        await loadArmTranslation()
        // The list only helps to choose; the kinds still work without it.
        gpus = (try? await service.gpus()) ?? []
    }

    public func setRenderGpu(_ value: String) async {
        if value.caseInsensitiveCompare(renderGpu) == .orderedSame && loaded {
            return
        }
        await change { try await service.setRenderGpu(value) }
    }

    public func setFreeze(_ on: Bool) async {
        if on == freeze && loaded { return }
        await change { try await service.setFreeze(on) }
    }

    public func setLinksInAndroid(_ on: Bool) async {
        if on == linksInAndroid && loaded { return }
        await change { try await service.setLinksInAndroid(on) }
    }

    public func setClipboard(_ on: Bool) async {
        if on == clipboard && loaded { return }
        await change { try await service.setClipboard(on) }
    }

    public func setFileSharing(_ on: Bool) async {
        if on == fileSharing && loaded { return }
        await change { try await service.setFileSharing(on) }
    }

    /// Shares `folder` or stops sharing it, keeping the other choices. A folder that cannot be
    /// shared changes nothing.
    public func setFolderShared(_ folder: String, _ shared: Bool) async {
        if !SettingsInfo.allFolders.contains(folder) || (shared == isFolderShared(folder) && loaded) {
            return
        }
        let wanted = SettingsInfo.allFolders.filter { $0 == folder ? shared : isFolderShared($0) }
        await change { try await service.setSharedFolders(wanted) }
    }

    /// Opens Android's shared storage in the Finder. A failure shows as the page's error.
    public func openAndroidFiles() async {
        error = nil
        do {
            _ = try await service.openAndroidFiles()
        } catch {
            self.error = Format.problem(text, error)
        }
    }

    public func setDeveloperMode(_ on: Bool) async {
        if on == developerMode && loaded { return }
        await change { try await service.setDeveloperMode(on) }
    }

    public func setAdbAutoConnect(_ on: Bool) async {
        if on == adbAutoConnect && loaded { return }
        await change { try await service.setAdbAutoConnect(on) }
    }

    /// Reads where auto-connect stands again, as the page does every few seconds while it is
    /// shown and developer mode is on. Only the state is taken, so a port being typed is kept.
    public func pollAdbAutoConnect() async {
        guard !isBusy, let kept = settings, kept.developerMode else { return }
        guard let now = try? await service.settings() else { return }
        guard !isBusy, var current = settings, now.adbAutoConnectState != current.adbAutoConnectState else { return }
        current.adbAutoConnectState = now.adbAutoConnectState
        settings = current
    }

    /// Picks the loopback port adb connects to. Text that is not a port from 1 to 65535
    /// changes nothing.
    public func setAdbPort(_ typed: String) async {
        let trimmed = typed.trimmingCharacters(in: .whitespaces)
        guard !trimmed.isEmpty, trimmed.allSatisfy(\.isASCII), trimmed.allSatisfy(\.isNumber),
              let port = Int(trimmed), (1...65535).contains(port) else {
            return
        }
        if port == adbPort && loaded { return }
        await change { try await service.setAdbPort(port) }
    }

    public func setArmTranslation(_ on: Bool) async {
        if on == armTranslation && loaded { return }
        await change { try await service.setArmTranslation(on) }
        await loadArmTranslation()
    }

    public func removeArmTranslation() async {
        isBusy = true
        error = nil
        do {
            arm = try await service.removeArmTranslation()
        } catch {
            self.error = Format.problem(text, error)
        }
        isBusy = false
    }

    /// Goes back to the image's default translator: the package installed in its place is
    /// removed, from the next start of Android.
    public func useDefaultArmTranslator() async {
        isBusy = true
        error = nil
        do {
            arm = try await service.useDefaultArmTranslator()
        } catch {
            self.error = Format.problem(text, error)
        }
        isBusy = false
    }

    /// Fetches and installs Google's ARM translator, once the user has agreed to Google's terms.
    /// A refusal (a download whose hash differs, no image for this Android) leaves the section
    /// as the service last reported it, with the reason shown.
    public func fetchGoogleArmTranslator() async {
        isBusy = true
        isFetchingGoogleArm = true
        error = nil
        do {
            arm = try await service.fetchGoogleArmTranslator()
        } catch {
            self.error = Format.problem(text, error)
        }
        isFetchingGoogleArm = false
        isBusy = false
    }

    public func setProgramUpdates(_ on: Bool) async {
        if on == programUpdates && loaded { return }
        await change { try await service.setProgramUpdates(on) }
    }

    public func setImageUpdates(_ on: Bool) async {
        if on == imageUpdates && loaded { return }
        await change { try await service.setImageUpdates(on) }
    }

    /// Sets the limit, in MiB (0 for automatic). A size below the smallest the service takes
    /// changes nothing.
    public func setMemoryLimit(_ mb: Int) async {
        if mb < 0 || (mb > 0 && mb < SettingsViewModel.smallestMemoryLimitMb) { return }
        if mb == memoryLimitMb && loaded { return }
        await change { try await service.setMemoryLimit(mb) }
    }

    public func setInputTrace(_ on: Bool) async {
        if on == inputTrace && loaded { return }
        await change { try await service.setInputTrace(on) }
    }

    public func setResizeTrace(_ on: Bool) async {
        if on == resizeTrace && loaded { return }
        await change { try await service.setResizeTrace(on) }
    }

    /// Reads Android's network again. A service that cannot say leaves what the settings
    /// reported.
    public func refreshNetwork() async {
        readNetwork = try? await service.network()
    }

    public func setAdvancedNetworking(_ on: Bool) async {
        if on == advancedNetworking && loaded { return }
        // The settings answer carries the network as it stands, which the page shows next.
        readNetwork = nil
        await change { try await service.setAdvancedNetworking(on) }
    }

    /// Erases Android's data through the service. The page asks the user first, with
    /// `resetWarning`; this does not ask again.
    public func resetData() async {
        isResetting = true
        resetDone = false
        error = nil
        do {
            try await service.resetData()
            resetDone = true
        } catch {
            self.error = Format.problem(text, error)
        }
        isResetting = false
    }

    /// Shows what failed while changing a setting another way, such as as an administrator.
    public func show(error message: String?) {
        error = message
    }
}
