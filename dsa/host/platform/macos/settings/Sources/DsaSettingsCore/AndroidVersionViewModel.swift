import Foundation

/// What `dsa android version --json` reports.
public struct AndroidVersionInfo: Equatable, Sendable {
    public var version: String
    public var major: Int
    public var buildId: String?
    public var securityPatch: String?
    public var channel: String
    public var supported: [Int]
    public var targetMajor: Int
    public var onTrial: Bool
    public var notice: String?
    /// The downloaded image the user's running Android waits to restart into, or nil.
    public var restartPending: String? = nil

    public static func parse(_ text: String) throws -> AndroidVersionInfo {
        let json = try Json.parse(text)
        guard json.isObject, json["android_major"].number != nil, json["target_major"].number != nil,
              json["supported_majors"].isArray else {
            throw DsaError(.unreadable, "dsa android version printed no android_major, target_major or supported_majors")
        }
        return AndroidVersionInfo(
            version: json.string("version"), major: json.int("android_major"), buildId: json.optionalString("build_id"),
            securityPatch: json.optionalString("security_patch"), channel: json.optionalString("channel") ?? "latest",
            supported: json["supported_majors"].items.compactMap { $0.number?.intValue },
            targetMajor: json.int("target_major"), onTrial: json.bool("on_trial"), notice: json.optionalString("notice"),
            restartPending: json.optionalString("restart_pending").flatMap { $0.isEmpty ? nil : $0 })
    }
}

/// One choice on the Android page: the newest version, or one major version.
public struct AndroidVersionChoice: Equatable, Hashable, Sendable {
    public var channel: String
    public var major: Int
    public var label: String
}

/// How moving to a choice treats the user's data.
public enum AndroidMove: Sendable {
    /// The choice leads to the major that runs now.
    case same
    /// A newer major: apps and their data move along.
    case up
    /// An older major: every user's Android data is erased.
    case down
}

/// The Android part of the Updates page: the version that runs (major, build, security patch),
/// the version it follows, where its image update stands, and the supported major versions to
/// choose from.
@MainActor
public final class AndroidVersionViewModel: ObservableObject {
    private let cli: DsaCli
    private let text: TextSource

    @Published public private(set) var info: AndroidVersionInfo?
    @Published public private(set) var choices: [AndroidVersionChoice] = []
    @Published public private(set) var error: String?
    @Published public private(set) var isBusy = false
    /// The channel of the choice the service keeps, or of the one the user just picked.
    @Published public var selectedChannel: String?

    public init(cli: DsaCli, text: TextSource) {
        self.cli = cli
        self.text = text
    }

    public var isLoaded: Bool { info != nil }
    /// Whether an Android image is installed. An installation without a bundled image reports
    /// the name "none" with Android 0 until its first image is downloaded; that is no Android
    /// version, so the page says no image is installed and leaves out the build and security
    /// patch rows instead of showing "Android 0" and "Unknown".
    public var hasImage: Bool { (info?.major ?? 0) > 0 }
    public var versionText: String {
        guard let info else {
            return ""
        }
        return hasImage ? text.format("Android_Major", info.major) : text.get("Android_NoImage")
    }
    public var imageVersion: String { hasImage ? (info?.version ?? "") : "" }
    public var buildId: String { info?.buildId ?? text.get("Android_Unknown") }
    public var securityPatch: String { info?.securityPatch ?? text.get("Android_Unknown") }
    public var supportedText: String {
        info.map { text.format("Android_Supported", $0.supported.map(String.init).joined(separator: ", ")) } ?? ""
    }
    public var notice: String { info?.notice ?? "" }
    public var hasNotice: Bool { !(info?.notice ?? "").isEmpty }
    /// A switch waits for its image: it is used from the next start.
    public var isSwitching: Bool { info.map { $0.targetMajor != $0.major } ?? false }
    public var switchingText: String { info.map { text.format("Android_Switching", $0.targetMajor) } ?? "" }

    public var selected: AndroidVersionChoice? { choices.first { $0.channel == selectedChannel } }

    /// The version the image updates follow: the newest supported one, or one major version and
    /// its security updates.
    public var channelText: String {
        guard let info else {
            return ""
        }
        if info.channel == "latest" {
            return text.format("Android_FollowsLatest", info.supported.max() ?? info.major)
        }
        return text.format("Android_FollowsMajor", info.channel)
    }

    /// How far the image download has got, from the service's notice ("downloading Android
    /// ...: 45% ..."), or nil when nothing is downloading.
    public var downloadPercent: Int? { AndroidVersionViewModel.downloadPercent(of: info?.notice) }
    public var isDownloading: Bool { downloadPercent != nil }

    /// Where the Android image update stands, in one line.
    public var updateStatusText: String {
        guard info != nil else {
            return ""
        }
        if isDownloading {
            return notice
        }
        if let restartPending {
            return text.format("Android_RestartPending", restartPending)
        }
        if hasNotice {
            return notice
        }
        return isSwitching ? switchingText : text.get("Android_UpToDate")
    }

    /// The downloaded image the user's running Android waits to restart into, or nil.
    public var restartPending: String? { info?.restartPending }
    /// Whether the [Restart now] button is shown.
    public var isRestartPending: Bool { restartPending != nil }
    /// Whether [Restart now] can be pressed now.
    public var canRestartNow: Bool { !isBusy && isRestartPending }

    /// The dsa arguments that restart Android now into the image that waits.
    public nonisolated static let restartArguments = ["android", "restart", "--json"]

    /// Restarts Android now into the downloaded image that waits for it, then reads the
    /// version again.
    public func restartNow() async { await run(AndroidVersionViewModel.restartArguments, thenRead: true) }

    /// Whether the service can be asked to check for an Android image now.
    public var canCheckNow: Bool { !isBusy && info != nil }

    /// The dsa arguments that ask for a check now: choosing the version the service already
    /// follows, which checks the signed image list at once and changes nothing else.
    public var checkArguments: [String] { ["android", "use", info?.channel ?? "latest", "--json"] }

    /// The percentage in a download notice, or nil.
    public nonisolated static func downloadPercent(of notice: String?) -> Int? {
        guard let notice, notice.hasPrefix("downloading Android "),
              let colon = notice.range(of: ": ") else {
            return nil
        }
        let after = notice[colon.upperBound...]
        let digits = after.prefix { $0.isASCII && $0.isNumber }
        guard !digits.isEmpty, after.dropFirst(digits.count).hasPrefix("%"), let percent = Int(digits) else {
            return nil
        }
        return min(max(percent, 0), 100)
    }

    /// Whether the picked choice differs from the one the service keeps.
    public var canApply: Bool {
        guard !isBusy, let info, let choice = selected else {
            return false
        }
        return choice.channel != info.channel
    }

    public var selectedMove: AndroidMove { selected.map(moveTo) ?? .same }

    /// How moving to `choice` treats the data.
    public func moveTo(_ choice: AndroidVersionChoice) -> AndroidMove {
        guard let info, choice.major != info.major else {
            return .same
        }
        return choice.major > info.major ? .up : .down
    }

    /// The dsa arguments that choose `choice`.
    public nonisolated static func useArguments(_ choice: AndroidVersionChoice, eraseData: Bool) -> [String] {
        eraseData
            ? ["android", "use", choice.channel, "--erase-data", "--json"]
            : ["android", "use", choice.channel, "--json"]
    }

    /// What the confirmation says before moving to an older major.
    public func downWarning(_ choice: AndroidVersionChoice) -> String {
        text.format("Android_DownWarning", choice.major, info?.major ?? 0)
    }

    /// What the confirmation says before moving to a newer major.
    public func upWarning(_ choice: AndroidVersionChoice) -> String {
        text.format("AndroidUpConfirm.Body", choice.major)
    }

    public func load() async { await run(["android", "version", "--json"]) }

    /// Reads the version again without showing the page as busy, for following a download
    /// while the page is shown.
    public func refresh() async {
        guard !isBusy, let result = try? await cli.run(["android", "version", "--json"]), result.exitCode == 0,
              let info = try? AndroidVersionInfo.parse(result.output), !isBusy else {
            return
        }
        let picked = selectedChannel
        show(info)
        if let picked, choices.contains(where: { $0.channel == picked }) {
            selectedChannel = picked
        }
    }

    /// Asks the service to check for an Android image now.
    public func checkNow() async { await run(checkArguments) }

    /// Chooses `choice`; `eraseData` is the user's agreement that an older major erases the
    /// data.
    public func use(_ choice: AndroidVersionChoice, eraseData: Bool) async {
        await run(AndroidVersionViewModel.useArguments(choice, eraseData: eraseData))
    }

    private func run(_ args: [String], thenRead: Bool = false) async {
        isBusy = true
        do {
            var result = try await cli.run(args)
            if result.exitCode != 0 {
                throw DsaError.failure(result)
            }
            if thenRead {
                result = try await cli.run(["android", "version", "--json"])
                if result.exitCode != 0 {
                    throw DsaError.failure(result)
                }
            }
            show(try AndroidVersionInfo.parse(result.output))
            error = nil
        } catch {
            self.error = Format.problem(text, error)
        }
        isBusy = false
    }

    private func show(_ info: AndroidVersionInfo) {
        self.info = info
        let newest = info.supported.max() ?? info.major
        choices = [AndroidVersionChoice(channel: "latest", major: newest, label: text.format("Android_ChoiceLatest", newest))]
            + info.supported.sorted(by: >).map {
                AndroidVersionChoice(channel: String($0), major: $0, label: text.format("Android_ChoiceMajor", $0))
            }
        selectedChannel = choices.first { $0.channel == info.channel }?.channel
    }
}
