import Foundation

/// A widget on offer as the Widgets page lists it.
public struct WidgetOfferRow: Equatable, Identifiable, Sendable {
    public var provider, label, app, detail: String
    /// The widget's preview, else its app's icon; nil when it has neither.
    public var preview: Data?
    public var id: String { provider }
}

/// A placed widget as the Widgets page lists it.
public struct PlacedWidgetRow: Equatable, Identifiable, Sendable {
    public var id: Int
    public var label, app, detail: String
}

/// The Widgets page: the widgets the apps in Android offer, which the user places on the
/// desktop, and the ones placed, which the user removes. While the page watches, a widget
/// placed or removed anywhere (its window's menu, dsa) shows here at once.
@MainActor
public final class WidgetsViewModel: ObservableObject {
    static let watchArgs = ["widgets", "watch", "--json"]

    private let service: DsaService
    private let text: TextSource
    private let stream: LogStream?

    @Published public private(set) var list: WidgetList?
    @Published public private(set) var error: String?
    @Published public private(set) var message: String?
    @Published public private(set) var isBusy = false
    /// The icons of the apps whose widgets have no preview, by package, which stand in for it.
    @Published public private(set) var appIcons: [String: Data] = [:]

    public init(service: DsaService, text: TextSource, stream: LogStream? = nil) {
        self.service = service
        self.text = text
        self.stream = stream
    }

    public var hasNoOffers: Bool { list?.offers.isEmpty == true }
    public var hasNoPlaced: Bool { list?.placed.isEmpty == true }

    private static func appName(_ appLabel: String, _ package: String) -> String { appLabel.isEmpty ? package : appLabel }

    private static func packageOf(_ provider: String) -> String {
        guard let slash = provider.firstIndex(of: "/"), slash > provider.startIndex else {
            return provider
        }
        return String(provider[..<slash])
    }

    public var offers: [WidgetOfferRow] {
        (list?.offers ?? []).map {
            WidgetOfferRow(
                provider: $0.provider, label: $0.label.isEmpty ? $0.provider : $0.label,
                app: WidgetsViewModel.appName($0.appLabel, $0.package),
                detail: text.format("Widgets_Size", $0.defaultWidthDp, $0.defaultHeightDp)
                    + ($0.needsConfiguration ? " · " + text.get("Widgets_Setup") : ""),
                preview: !$0.previewPng.isEmpty ? $0.previewPng : appIcons[WidgetsViewModel.packageOf($0)])
        }
    }

    /// The package of the app that offers `offer`: its own, else its provider's.
    private static func packageOf(_ offer: WidgetOffer) -> String {
        offer.package.isEmpty ? packageOf(offer.provider) : offer.package
    }

    /// Reads the icons of the apps whose widgets have no preview, when the page has none of
    /// them yet. A list it cannot read leaves the stand-in icon.
    private func readAppIcons() async {
        let wanted = Set((list?.offers ?? []).filter { $0.previewPng.isEmpty }.map { WidgetsViewModel.packageOf($0) })
        if wanted.isEmpty || wanted.isSubset(of: Set(appIcons.keys)) {
            return
        }
        guard let apps = try? await service.runJson(["apps", "list"]) else {
            return
        }
        var icons = appIcons
        for app in apps["apps"].items {
            let package = app.string("package")
            let icon = app.base64("icon_png")
            if wanted.contains(package) && !icon.isEmpty {
                icons[package] = icon
            }
        }
        appIcons = icons
    }

    public var placed: [PlacedWidgetRow] {
        (list?.placed ?? []).map {
            PlacedWidgetRow(
                id: $0.id, label: $0.label.isEmpty ? $0.provider : $0.label,
                app: WidgetsViewModel.appName($0.appLabel, WidgetsViewModel.packageOf($0.provider)),
                detail: text.format("Widgets_At", $0.x, $0.y, $0.widthDp, $0.heightDp))
        }
    }

    private func run(_ work: () async throws -> String?) async {
        if isBusy {
            return
        }
        isBusy = true
        message = nil
        do {
            message = try await work()
            list = try await service.widgets()
            error = nil
            await readAppIcons()
        } catch {
            self.error = Format.problem(text, error)
        }
        isBusy = false
    }

    /// Reads the widgets on offer and the ones placed.
    public func refresh() async { await run { nil } }

    /// Places a widget of `provider` on the desktop.
    public func add(_ provider: String) async {
        await run {
            let placed = try await service.addWidget(provider)
            return text.format("Widgets_Placed", placed.label.isEmpty ? placed.provider : placed.label)
        }
    }

    /// Removes the widget `id` from the desktop.
    public func remove(_ id: Int) async {
        await run {
            let label = list?.placed.first(where: { $0.id == id })?.label ?? String(id)
            try await service.removeWidget(id)
            return text.format("Widgets_Removed", label)
        }
    }

    /// Applies a change that came while watching to the placed widgets shown. The widgets on
    /// offer stay as they were read.
    public func apply(_ change: WidgetChange) {
        var placed = list?.placed ?? []
        switch change {
        case .all(let widgets):
            placed = widgets
        case .placed(let widget):
            if let at = placed.firstIndex(where: { $0.id == widget.id }) {
                placed[at] = widget
            } else {
                placed.append(widget)
                placed.sort { $0.id < $1.id }
            }
        case .removed(let id):
            placed.removeAll { $0.id == id }
        }
        list = WidgetList(offers: list?.offers ?? [], placed: placed)
    }

    /// Watches the placed widgets once, through `dsa widgets watch`, until it ends or the task
    /// is cancelled. Does nothing without a stream.
    public func watchOnce() async throws {
        guard let stream else {
            return
        }
        for try await line in stream.lines(WidgetsViewModel.watchArgs) {
            if let change = DsaService.parseWidgetChange(line) {
                apply(change)
            }
        }
    }

    /// Watches the placed widgets until the task is cancelled, watching again after `retry`
    /// seconds whenever the watch ends, as it does when Android stops.
    public func watch(retry: Double) async {
        while !Task.isCancelled {
            do {
                try await watchOnce()
            } catch {
                // Android stopped or the service is away: the page keeps what it shows, and
                // the watch starts again below.
            }
            if Task.isCancelled {
                return
            }
            try? await Task.sleep(nanoseconds: UInt64(retry * 1_000_000_000))
        }
    }
}
