import Foundation

/// The General page: the state of Android, starting, stopping and freezing it, and the last
/// error.
@MainActor
public final class GeneralViewModel: ObservableObject {
    private let service: DsaService
    private let text: TextSource

    @Published public private(set) var status: StatusInfo?
    @Published public private(set) var isBusy = false
    @Published public private(set) var error: String?

    public init(service: DsaService, text: TextSource) {
        self.service = service
        self.text = text
    }

    public var state: SessionState { status?.state ?? .unknown }
    public var stateText: String { text.get("State_\(state.rawValue)") }
    /// The service's last reason. A start this computer refused reads as a host problem in
    /// the user's language, with the reason the host gave; the image is not to blame for it.
    public var lastReason: String {
        let reason = status?.lastReason ?? ""
        let first = reason.split(separator: "\n", maxSplits: 1, omittingEmptySubsequences: false)
            .first.map(String.init) ?? ""
        if first.hasPrefix(GeneralViewModel.hostProblemPrefix) {
            return text.format("LastReason_Host", String(first.dropFirst(GeneralViewModel.hostProblemPrefix.count)))
        }
        return reason
    }

    /// What the service's account of a start the host refused starts with.
    public static let hostProblemPrefix =
        "this computer could not run Android (a problem with the host, not with Android's image): "
    public var hasLastReason: Bool { !(status?.lastReason ?? "").isEmpty }
    public var problem: String { status?.problem ?? "" }
    public var hasProblem: Bool { !(status?.problem ?? "").isEmpty }
    public var updateNotice: String { status?.updateNotice ?? "" }
    public var hasUpdateNotice: Bool { !(status?.updateNotice ?? "").isEmpty }
    public var canStart: Bool { startButton == .start }

    /// What this app is doing with the first button: `.start`, `.restart` or `.locked`.
    @Published public private(set) var action: StartButtonKind = .locked

    /// The status card's first button. The service decides it from the state; while this app
    /// starts or restarts Android, it stays locked with the label of what it does, as the state
    /// passes through stopping and starting.
    public var startButton: StartButtonKind {
        switch action {
        case .start: return .starting
        case .restart: return .restarting
        default: return isBusy ? .locked : (status?.startButton ?? .locked)
        }
    }
    /// Whether the first button reads Restart rather than Start.
    public var isRestart: Bool { startButton == .restart || startButton == .restarting }
    public var startButtonText: String { text.get(isRestart ? "RestartButton.Content" : "StartButton.Content") }
    /// Whether pressing the first button does something now.
    public var canPressStart: Bool { startButton == .start || startButton == .restart }
    /// Whether progress shows beside the state: the first button is locked while a start, a
    /// restart or a stop runs.
    public var showsProgress: Bool {
        isBusy || [.starting, .restarting, .stopping].contains(startButton)
    }
    /// Whether pressing it first asks, since a restart closes every app window.
    public var startConfirms: Bool { startButton == .restart }
    public var canStop: Bool {
        !isBusy && [.starting, .stalled, .active, .dozing, .frozen].contains(state)
    }
    /// Whether Android runs and can be frozen now.
    public var canFreeze: Bool { !isBusy && [.active, .dozing].contains(state) }

    private func run(_ action: () async throws -> StatusInfo) async {
        isBusy = true
        error = nil
        do {
            status = try await action()
        } catch {
            self.error = Format.problem(text, error)
        }
        isBusy = false
    }

    public func refresh() async { await run { try await service.status() } }

    /// Whether an update of DSA is under way. Meanwhile a poll asks a running service only:
    /// setup stops the service, and starting it again would run the old version under setup.
    public var updating: (() -> Bool)?

    /// Reads the state again without marking the page busy, as a timer does while the page is
    /// shown. Does nothing while a start or a stop is under way.
    public func poll() async {
        if isBusy {
            return
        }
        do {
            let now: StatusInfo
            if updating?() == true {
                now = try await service.runningStatus()
            } else {
                now = try await service.status()
            }
            if isBusy {
                return
            }
            status = now
            error = nil
        } catch {
            self.error = Format.problem(text, error)
        }
    }

    public func start() async { await press(.start) { try await service.start() } }
    /// Restarts Android, as `dsa android restart` does. The page asks the user first, since
    /// its app windows close.
    public func restart() async { await press(.restart) { try await service.restart() } }

    /// What the first button does when pressed: start a stopped Android or restart a running
    /// one. Nothing while it is locked.
    public func pressStart() async {
        switch startButton {
        case .start: await start()
        case .restart: await restart()
        default: return
        }
    }

    private func press(_ kind: StartButtonKind, _ act: () async throws -> StatusInfo) async {
        action = kind
        await run(act)
        action = .locked
    }
    public func stop() async { await run { try await service.stop() } }
    /// Stops every process of Android until one of its windows is shown again.
    public func freeze() async { await run { try await service.freeze() } }
}
