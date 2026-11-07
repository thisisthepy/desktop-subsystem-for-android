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
    public var lastReason: String { status?.lastReason ?? "" }
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

    /// Reads the state again without marking the page busy, as a timer does while the page is
    /// shown. Does nothing while a start or a stop is under way.
    public func poll() async {
        if isBusy {
            return
        }
        do {
            let now = try await service.status()
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
