import AppKit
import DsaSettingsCore
import SwiftUI
import UniformTypeIdentifiers

/// The pages, in the order the sidebar lists them, as the Windows app lists its own.
enum SettingsPage: String, CaseIterable, Identifiable {
    case general, memory, apps, background, widgets, display, identity, network, updates, advanced, logs, about

    var id: String { rawValue }

    /// The pages in the sidebar's last group, as the Windows app keeps them in its pane's
    /// footer: Logs directly above About.
    static let footer: [SettingsPage] = [.logs, .about]

    /// The pages above that group.
    static var main: [SettingsPage] { allCases.filter { !footer.contains($0) } }

    var titleKey: String {
        switch self {
        case .general: return "NavGeneral.Content"
        case .memory: return "NavMemory.Content"
        case .apps: return "NavApps.Content"
        case .background: return "NavBackground.Content"
        case .widgets: return "NavWidgets.Content"
        case .display: return "NavDisplay.Content"
        case .updates: return "NavUpdates.Content"
        case .identity: return "NavIdentity.Content"
        case .network: return "NavNetwork.Content"
        case .advanced: return "NavAdvanced.Content"
        case .logs: return "NavLogs.Content"
        case .about: return "NavAbout.Content"
        }
    }

    /// The SF Symbol and the tile colour, as System Settings shows each pane.
    var symbol: (String, Color) {
        switch self {
        case .general: return ("gearshape", .gray)
        case .memory: return ("memorychip", .green)
        case .apps: return ("square.grid.2x2", .blue)
        case .background: return ("play.fill", .orange)
        case .widgets: return ("rectangle.3.group", .indigo)
        case .display: return ("display", .blue)
        case .updates: return ("arrow.triangle.2.circlepath", .gray)
        case .identity: return ("person.text.rectangle", .teal)
        case .network: return ("network", .blue)
        case .advanced: return ("slider.horizontal.3", .gray)
        case .logs: return ("doc.text", .brown)
        case .about: return ("info.circle", .gray)
        }
    }

    /// Picks the page from its name, as `--page <name>` gives it.
    static func named(_ name: String) -> SettingsPage? {
        switch name.lowercased() {
        case "device": return .identity
        // The Android page is part of Updates now; its old name still opens it.
        case "android": return .updates
        default: return SettingsPage(rawValue: name.lowercased())
        }
    }
}

/// A sidebar entry: the page's symbol on a coloured tile, then its name.
struct SidebarLabel: View {
    let page: SettingsPage

    var body: some View {
        Label {
            Text(T(page.titleKey))
        } icon: {
            Image(systemName: page.symbol.0)
                .font(.system(size: 11, weight: .semibold))
                .foregroundStyle(.white)
                .frame(width: 20, height: 20)
                .background(RoundedRectangle(cornerRadius: 5, style: .continuous).fill(page.symbol.1.gradient))
        }
    }
}

/// The settings window: a sidebar of pages and the chosen page, as System Settings lays itself
/// out. APK files dropped anywhere on it are installed in Android.
struct SettingsWindow: View {
    static let id = "settings"

    @EnvironmentObject private var model: AppModel
    @State private var page: SettingsPage? = SettingsWindow.startPage
    @State private var dropTargeted = false

    /// The page to open on: `--page <name>`, else General.
    static var startPage: SettingsPage {
        let args = CommandLine.arguments
        if let at = args.firstIndex(of: "--page"), at + 1 < args.count, let page = SettingsPage.named(args[at + 1]) {
            return page
        }
        return .general
    }

    var body: some View {
        NavigationSplitView {
            List(selection: $page) {
                Section {
                    ForEach(SettingsPage.main) { page in
                        SidebarLabel(page: page).tag(page)
                    }
                }
                Section {
                    ForEach(SettingsPage.footer) { page in
                        SidebarLabel(page: page).tag(page)
                    }
                }
            }
            .listStyle(.sidebar)
            // As wide as the longest label needs: the list's and the row's insets, the 20 pt tile,
            // the label spacing and "Running in the background" at 13 pt, rounded up to 4.
            .navigationSplitViewColumnWidth(min: 224, ideal: 224, max: 300)
        } detail: {
            VStack(spacing: 0) {
                InstallBar(install: model.install)
                detail
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
            .navigationTitle(T((page ?? .general).titleKey))
        }
        .onDrop(of: [.fileURL], isTargeted: $dropTargeted) { providers in
            drop(providers)
        }
        .overlay {
            if dropTargeted {
                RoundedRectangle(cornerRadius: 10)
                    .strokeBorder(Color.accentColor, lineWidth: 3)
                    .overlay(alignment: .bottom) {
                        Text(T("Install_DropHint"))
                            .padding(8)
                            .background(.regularMaterial, in: Capsule())
                            .padding()
                    }
                    .allowsHitTesting(false)
            }
        }
        .task { await model.settings.load() }
        // App Info in a task window's Window menu: the Apps page, searched for that app so its
        // row is the one in view.
        .onOpenURL { url in
            // A click on DSA's notification about an update: the Updates page.
            if AppPageLink.isUpdates(url) {
                page = .updates
                NSApp.activate(ignoringOtherApps: true)
                return
            }
            guard let package = AppPageLink.package(of: url) else { return }
            page = .apps
            model.apps.search = package
            NSApp.activate(ignoringOtherApps: true)
        }
    }

    @ViewBuilder private var detail: some View {
        switch page ?? .general {
        case .general: GeneralPage(vm: model.general, settings: model.settings)
        case .memory: MemoryPage(vm: model.memory, settings: model.settings)
        case .apps: AppsPage(vm: model.apps)
        case .background: BackgroundPage(vm: model.background)
        case .widgets: WidgetsPage(vm: model.widgets)
        case .display: DisplayPage(vm: model.settings)
        case .updates: UpdatesPage(vm: model.settings, android: model.android, general: model.general, update: model.programUpdate)
        case .identity: DeviceIdentityPage(vm: model.identity)
        case .network: NetworkPage(vm: model.settings)
        case .advanced: AdvancedPage(vm: model.settings, gapps: model.gapps, widevine: model.widevine, backup: model.backup)
        case .logs: LogsPage(vm: model.logs)
        case .about: AboutPage(vm: model.settings)
        }
    }

    /// Reads the files of a drop and installs the APKs among them.
    private func drop(_ providers: [NSItemProvider]) -> Bool {
        let install = model.install
        let group = DispatchGroup()
        let found = DroppedPaths()
        for provider in providers where provider.hasItemConformingToTypeIdentifier(UTType.fileURL.identifier) {
            group.enter()
            _ = provider.loadObject(ofClass: URL.self) { url, _ in
                if let url, url.isFileURL {
                    found.add(url.path)
                }
                group.leave()
            }
        }
        group.notify(queue: .main) {
            Task { @MainActor in
                let paths = found.all
                if !paths.isEmpty {
                    await install.drop(paths)
                }
            }
        }
        return !providers.isEmpty
    }
}

/// The bar at the top of the window that says how installing dropped APKs went.
struct InstallBar: View {
    @ObservedObject var install: InstallViewModel

    var body: some View {
        if install.isOpen {
            HStack(alignment: .top, spacing: 10) {
                Banner(kind: kind, title: install.title, message: install.message)
                Spacer(minLength: 0)
                if install.isBusy {
                    ProgressView().controlSize(.small)
                } else {
                    Button {
                        install.isOpen = false
                    } label: {
                        Image(systemName: "xmark.circle.fill")
                    }
                    .buttonStyle(.borderless)
                    .accessibilityLabel(T("CancelButton"))
                }
            }
            .padding(12)
            .background(.quaternary.opacity(0.5), in: RoundedRectangle(cornerRadius: 8))
            .padding([.horizontal, .top], 20)
        }
    }

    private var kind: Banner.Kind {
        switch install.severity {
        case .success: return .success
        case .error: return .error
        case .informational: return .info
        }
    }
}

/// The paths of a drop, gathered as each item loads.
private final class DroppedPaths: @unchecked Sendable {
    private let lock = NSLock()
    private var paths: [String] = []

    func add(_ path: String) {
        lock.lock()
        paths.append(path)
        lock.unlock()
    }

    var all: [String] {
        lock.lock()
        defer { lock.unlock() }
        return paths
    }
}
