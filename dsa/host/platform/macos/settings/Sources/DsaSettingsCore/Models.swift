import Foundation

/// Where the user's Android is in its life, as the service reports it.
public enum SessionState: String, CaseIterable, Sendable {
    case unknown = "Unknown"
    case stopped = "Stopped"
    case starting = "Starting"
    case active = "Active"
    case dozing = "Dozing"
    case frozen = "Frozen"
    case stopping = "Stopping"
    case stalled = "Stalled"

    public static func parse(_ state: String?) -> SessionState {
        guard let state, let known = SessionState(rawValue: state), known != .unknown else {
            return .unknown
        }
        return known
    }
}

public struct StatusInfo: Equatable, Sendable {
    public var state: SessionState
    public var lastReason: String?
    public var updateNotice: String?
    public var problem: String?

    public init(state: SessionState, lastReason: String? = nil, updateNotice: String? = nil, problem: String? = nil) {
        self.state = state
        self.lastReason = lastReason
        self.updateNotice = updateNotice
        self.problem = problem
    }
}

/// Which GPU the window manager renders on.
public enum RenderGpuKind: Equatable, Sendable {
    /// The GPU that drives the main display (the default).
    case display
    case integrated
    case discrete
    /// The first GPU whose name contains the setting.
    case named
}

/// How Android reaches the network.
public enum NetworkMode: Equatable, Sendable {
    /// The service did not say.
    case unknown
    /// Behind a NAT network of its own.
    case nat
    /// On this computer's network as a device of its own.
    case advanced
    /// Android is not running, so it has no network yet.
    case off
}

/// Android's network as the service reports it.
public struct NetworkInfo: Equatable, Sendable {
    public var mode: NetworkMode
    public var addresses: [String]
    public var dnsServers: [String]
    public var gateway: String
    public var problem: String?

    public init(mode: NetworkMode, addresses: [String] = [], dnsServers: [String] = [], gateway: String = "", problem: String? = nil) {
        self.mode = mode
        self.addresses = addresses
        self.dnsServers = dnsServers
        self.gateway = gateway
        self.problem = problem
    }
}

/// Where attaching developer mode's port to this Mac's adb server stands.
public enum AdbAutoConnectState: String, CaseIterable, Equatable, Sendable {
    /// Developer mode or auto-connect is off.
    case off = "Off"
    /// No adb server runs on this Mac yet.
    case waitingForServer = "WaitingForServer"
    /// Android is not running, or adb in Android has not answered yet.
    case waitingForAndroid = "WaitingForAndroid"
    /// The adb server lists Android.
    case attached = "Attached"
    /// The user ran adb disconnect; it is attached again when the adb server restarts.
    case detached = "Detached"

    /// The state a name the service gives stands for. An empty or unknown name, as an older
    /// service gives, is off.
    public static func read(_ name: String?) -> AdbAutoConnectState {
        switch name {
        case "waiting-for-server": return .waitingForServer
        case "waiting-for-android": return .waitingForAndroid
        case "attached": return .attached
        case "detached": return .detached
        default: return .off
        }
    }

    /// The key of its text in the String Catalog.
    public var textKey: String { "AdbAutoConnect_\(rawValue)" }
}

public struct SettingsInfo: Equatable, Sendable {
    /// The folders that can be shared with Android, in the order the service keeps them.
    public static let allFolders = ["Documents", "Downloads", "Pictures", "Music", "Videos", "Desktop"]

    /// The loopback port adb connects to in developer mode unless the user picks another.
    public static let defaultAdbPort = 58526

    public var renderGpu: String = "display"
    public var freeze = false
    public var programUpdates = false
    public var imageUpdates = false
    public var programUpdatesAvailable = false
    public var imageUpdatesAvailable = false
    public var version = ""
    public var logDir: String?
    public var clipboard = true
    public var linksInAndroid = false
    public var androidVersion: String?
    public var developerMode = false
    public var adbPort = SettingsInfo.defaultAdbPort
    public var inputTrace = false
    public var resizeTrace = false
    public var memoryLimitMb = 0
    public var armTranslation = true
    public var stopWithoutAsking = false
    public var advancedNetworking = false
    public var network: NetworkInfo?
    public var fileSharing = true
    public var sharedFolders: [String]?
    /// Whether developer mode's port is attached to this Mac's adb server by itself.
    public var adbAutoConnect = true
    /// Where that stands, as the service last said.
    public var adbAutoConnectState: AdbAutoConnectState = .off

    public init() {}

    /// The folders shared while file sharing is on: every one unless the user chose fewer.
    public var folders: [String] { sharedFolders ?? SettingsInfo.allFolders }

    /// What to type to connect adb while developer mode is on.
    public var adbConnect: String { "adb connect 127.0.0.1:\(adbPort)" }

    public var renderGpuKind: RenderGpuKind {
        switch renderGpu.lowercased() {
        case "display": return .display
        case "integrated": return .integrated
        case "discrete": return .discrete
        default: return .named
        }
    }
}

/// An ARM translation package, as it describes itself.
public struct ArmPackageInfo: Equatable, Sendable {
    public var name, version, library, license: String
}

/// What the running Android says of ARM translation.
public struct ArmGuestInfo: Equatable, Sendable {
    public var installed, active: Bool
    public var abilist: String
}

/// Where an ARM translator comes from.
public enum ArmSource: String, Equatable, Sendable {
    /// The default translator the image of this Android carries.
    case builtIn = "default"
    /// A package the user installed.
    case user
    /// Google's translator, which the user fetched.
    case google
}

/// The ARM translator Android uses: where it comes from, its name, version and licence.
public struct ArmTranslatorInfo: Equatable, Sendable {
    public var source: ArmSource
    public var name, version, license: String
}

/// Whether apps built only for 64-bit ARM can run, whether Google's translator can be
/// fetched here (not on an ARM64 Mac, whose Android runs ARM apps itself), the default package
/// the image carries (only an x86_64 image has one), and the translator Android uses from its
/// next start.
public struct ArmTranslationInfo: Equatable, Sendable {
    public var package: ArmPackageInfo?
    public var enabled: Bool
    public var supported: Bool
    public var android: ArmGuestInfo?
    public var restartNeeded: Bool
    public var googleFetchAvailable = false
    public var defaultPackage: ArmPackageInfo? = nil
    public var translator: ArmTranslatorInfo? = nil
}

/// What a host process does for the user's Android.
public enum HostRole: String, Sendable {
    case vm = "Vm"
    case windowManager = "WindowManager"
    case appWindow = "AppWindow"

    static func parse(_ role: String?) -> HostRole? {
        switch role {
        case "vm": return .vm
        case "window manager": return .windowManager
        case "app window": return .appWindow
        default: return nil
        }
    }
}

public struct HostProcess: Equatable, Sendable {
    public var role: HostRole
    public var pid: UInt32
    public var workingSetKb: UInt64
    public var privateKb: UInt64
}

public struct GuestProcess: Equatable, Sendable {
    public var pid: Int
    public var name: String
    public var pssKb: UInt64
    public var swapPssKb: UInt64
}

/// What the guest kernel holds for itself, in KiB.
public struct GuestKernelInfo: Equatable, Sendable {
    public var slabUnreclaimableKb, kernelStackKb, pageTablesKb, vmallocUsedKb, percpuKb: UInt64
    public var cmaTotalKb, memOnlineKb, unaccountedKb: UInt64
    /// Nil when the guest could not count its memory.
    public var reservedAtBootKb: UInt64?
}

public struct GuestMemoryInfo: Equatable, Sendable {
    public var totalKb, freeKb, availableKb, buffersKb, cachedKb, anonPagesKb, shmemKb: UInt64
    public var slabReclaimableKb, swapTotalKb, swapFreeKb, zramOriginalKb, zramUsedKb, totalPssKb: UInt64
    public var topProcesses: [GuestProcess]
    public var ageSeconds: UInt64
    public var kernel: GuestKernelInfo?
}

/// The memory of the user's Android. `vmBeyondGuestKb` is what the VM holds on the host
/// beyond what Android uses, nil until Android has reported.
public struct MemoryReport: Equatable, Sendable {
    public var guest: GuestMemoryInfo?
    public var host: [HostProcess]
    public var hostTotalKb: UInt64
    public var vmBeyondGuestKb: UInt64?
}

public struct GpuInfo: Equatable, Sendable {
    public var name: String
    public var luid: String
    public var primaryDisplay: Bool
}

/// How installing an APK ended: `status` is `installed` or the stable reason dsa gives.
public struct InstallReport: Equatable, Sendable {
    public var installed: Bool
    public var status: String
    public var package: String?
    public var detail: String
}

/// A widget an app in Android offers. Sizes are in device-independent pixels.
public struct WidgetOffer: Equatable, Sendable {
    public var provider, package, label: String
    public var previewPng: Data
    public var defaultWidthDp, defaultHeightDp: Int
    public var needsConfiguration: Bool
    public var appLabel: String
}

/// A widget placed on the desktop.
public struct PlacedWidget: Equatable, Sendable {
    public var id: Int
    public var provider, label, appLabel: String
    public var x, y, widthDp, heightDp: Int
}

public struct WidgetList: Equatable, Sendable {
    public var offers: [WidgetOffer]
    public var placed: [PlacedWidget]
}

/// A change to the placed widgets, as `dsa widgets watch` reports it.
public enum WidgetChange: Equatable, Sendable {
    /// The widgets placed now, all of them.
    case all([PlacedWidget])
    /// A widget was placed, or described again.
    case placed(PlacedWidget)
    /// A widget was removed.
    case removed(Int)
}

/// An app that keeps running with no window because it runs a foreground service.
public struct BackgroundApp: Equatable, Sendable {
    public var package, label, title, text: String
    public var actions: [String]
    public var iconPng: Data
}

/// An app's active media session.
public struct MediaInfo: Equatable, Sendable {
    public var package, label, title, artist, album, playback: String
}

public struct BackgroundList: Equatable, Sendable {
    public var apps: [BackgroundApp]
    public var media: [MediaInfo]
}
