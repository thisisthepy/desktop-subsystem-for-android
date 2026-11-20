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
    public var canStart: Bool { !isBusy && state == .stopped }
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

    public func start() async { await run { try await service.start() } }
    public func stop() async { await run { try await service.stop() } }
    /// Stops every process of Android until one of its windows is shown again.
    public func freeze() async { await run { try await service.freeze() } }
}
