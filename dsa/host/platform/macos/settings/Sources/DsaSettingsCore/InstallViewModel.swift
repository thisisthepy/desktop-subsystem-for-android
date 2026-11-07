import Foundation

/// How the install bar looks.
public enum InstallSeverity: Sendable {
    case informational, success, error
}

/// APK files dropped on the window: each is installed in Android in turn, and the bar at the
/// top of the window says how it went.
@MainActor
public final class InstallViewModel: ObservableObject {
    private let service: DsaService
    private let text: TextSource

    @Published public private(set) var isBusy = false
    /// Whether the bar shows; its close button sets it to false.
    @Published public var isOpen = false
    @Published public private(set) var severity = InstallSeverity.informational
    @Published public private(set) var title = ""
    @Published public private(set) var message = ""

    public init(service: DsaService, text: TextSource) {
        self.service = service
        self.text = text
    }

    /// Whether `path` names what the install takes: an APK file, a split-APK bundle (`.xapk`,
    /// `.apks`, `.apkm`), or an App Bundle, which it refuses with its reason.
    public nonisolated static func isApk(_ path: String) -> Bool {
        let lower = path.lowercased()
        return [".apk", ".xapk", ".apks", ".apkm", ".aab"].contains { lower.hasSuffix($0) }
    }

    /// Whether a drop of `paths` would install anything, so the window shows a drop is welcome.
    public func accepts(_ paths: [String]) -> Bool { !isBusy && paths.contains(where: InstallViewModel.isApk) }

    private func show(_ severity: InstallSeverity, _ title: String, _ message: String) {
        self.severity = severity
        self.title = title
        self.message = message
        isOpen = true
    }

    private static func name(_ path: String) -> String { (path as NSString).lastPathComponent }

    /// Installs every APK of `paths`, one after the other, and says how each went. Files that
    /// are not APKs are named and left alone.
    public func drop(_ paths: [String]) async {
        if isBusy {
            return
        }
        let apks = paths.filter(InstallViewModel.isApk)
        let others = paths.filter { !InstallViewModel.isApk($0) }.map(InstallViewModel.name)
        if apks.isEmpty {
            show(.error, text.get("Install_FailedTitle"), text.format("Install_NotApk", others.joined(separator: ", ")))
            return
        }
        isBusy = true
        var lines: [String] = []
        var failed = false
        for path in apks {
            let name = InstallViewModel.name(path)
            show(.informational, text.get("Install_BusyTitle"), text.format("Install_Busy", name))
            do {
                let report = try await service.install(path)
                if report.installed {
                    lines.append(text.format("Install_Installed", name, report.package ?? ""))
                } else {
                    failed = true
                    lines.append(text.format("Install_Refused", name, text.get("Install_Reason_\(report.status)"), report.detail))
                }
            } catch {
                failed = true
                lines.append(text.format("Install_NotSent", name, Format.problem(text, error)))
            }
        }
        if !others.isEmpty {
            lines.append(text.format("Install_NotApk", others.joined(separator: ", ")))
        }
        isBusy = false
        show(failed ? .error : .success, text.get(failed ? "Install_FailedTitle" : "Install_DoneTitle"),
             lines.joined(separator: "\n"))
    }
}
