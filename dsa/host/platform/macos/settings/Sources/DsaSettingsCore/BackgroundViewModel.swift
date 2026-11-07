import Foundation

/// An app that runs in the background as the page lists it.
public struct BackgroundAppRow: Equatable, Identifiable, Sendable {
    public var package, label, detail: String
    public var icon: Data?
    public var id: String { package }
}

/// The "Running in the background" page: the apps in Android that keep running with no
/// window because they run a foreground service, each with a button that opens it and one
/// that stops it, and whether stopping one from the menu bar asks first. Reading the list
/// never starts Android.
@MainActor
public final class BackgroundViewModel: ObservableObject {
    private let service: DsaService
    private let text: TextSource

    @Published public private(set) var list: BackgroundList?
    /// Whether stopping an app from the menu bar asks first.
    @Published public private(set) var askBeforeStop = true
    @Published public private(set) var error: String?
    @Published public private(set) var message: String?
    @Published public private(set) var isBusy = false

    public init(service: DsaService, text: TextSource) {
        self.service = service
        self.text = text
    }

    public var hasNone: Bool { list?.apps.isEmpty == true }

    public var apps: [BackgroundAppRow] {
        (list?.apps ?? []).map {
            BackgroundAppRow(package: $0.package, label: $0.label.isEmpty ? $0.package : $0.label,
                             detail: detail($0), icon: $0.iconPng.isEmpty ? nil : $0.iconPng)
        }
    }

    /// The notification's title and text, and what the app plays when it has a media session.
    private func detail(_ app: BackgroundApp) -> String {
        var parts: [String] = []
        let said = [app.title, app.text]
            .filter { !$0.trimmingCharacters(in: .whitespaces).isEmpty }
            .joined(separator: " · ")
        if !said.isEmpty {
            parts.append(said)
        }
        if let media = list?.media.first(where: { $0.package == app.package }), !media.title.isEmpty {
            let key = media.playback == "playing" ? "Background_Playing" : "Background_Paused"
            let what = media.artist.isEmpty ? media.title : media.title + " - " + media.artist
            parts.append(text.format(key, what))
        }
        return parts.isEmpty ? app.package : parts.joined(separator: "\n")
    }

    private func run(_ work: () async throws -> String?) async {
        if isBusy {
            return
        }
        isBusy = true
        message = nil
        do {
            message = try await work()
            list = try await service.background()
            error = nil
        } catch {
            self.error = Format.problem(text, error)
        }
        isBusy = false
    }

    /// Reads the apps that run in the background, and whether a stop asks first.
    public func refresh() async {
        await run {
            let settings = try await service.settings()
            askBeforeStop = !settings.stopWithoutAsking
            return nil
        }
    }

    /// Stops `package`. The page's button is the user's own choice, so it does not ask again.
    public func stop(_ package: String) async {
        await run {
            let label: String
            if let app = list?.apps.first(where: { $0.package == package }), !app.label.isEmpty {
                label = app.label
            } else {
                label = package
            }
            try await service.stopBackground(package)
            return text.format("Background_Stopped", label)
        }
    }

    /// Opens the window of `package`.
    public func open(_ package: String) async {
        await run {
            try await service.openBackground(package)
            return nil
        }
    }

    /// Whether stopping an app from the menu bar asks first.
    public func setAskBeforeStop(_ ask: Bool) async {
        await run {
            let settings = try await service.setStopWithoutAsking(!ask)
            askBeforeStop = !settings.stopWithoutAsking
            return nil
        }
    }
}
