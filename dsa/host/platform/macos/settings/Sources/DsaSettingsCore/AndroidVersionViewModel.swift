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
            targetMajor: json.int("target_major"), onTrial: json.bool("on_trial"), notice: json.optionalString("notice"))
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

/// The Android page: the version that runs (major, build, security patch) and the supported
/// major versions to choose from.
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
    public var versionText: String { info.map { text.format("Android_Major", $0.major) } ?? "" }
    public var imageVersion: String { info?.version ?? "" }
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

    /// Chooses `choice`; `eraseData` is the user's agreement that an older major erases the
    /// data.
    public func use(_ choice: AndroidVersionChoice, eraseData: Bool) async {
        await run(AndroidVersionViewModel.useArguments(choice, eraseData: eraseData))
    }

    private func run(_ args: [String]) async {
        isBusy = true
        do {
            let result = try await cli.run(args)
            if result.exitCode != 0 {
                throw DsaError.failure(result)
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
