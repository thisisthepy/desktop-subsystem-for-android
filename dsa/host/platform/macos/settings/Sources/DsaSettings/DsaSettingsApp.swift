import AppKit
import DsaSettingsCore
import SwiftUI

/// Every page's view model, made once for the app, as the Windows app keeps them.
@MainActor
final class AppModel: ObservableObject {
    let cli: ProcessDsaCli
    let service: DsaService
    let general: GeneralViewModel
    let settings: SettingsViewModel
    let gapps: GappsViewModel
    let memory: MemoryViewModel
    let install: InstallViewModel
    let apps: AppsViewModel
    let widgets: WidgetsViewModel
    let background: BackgroundViewModel
    let identity: DeviceIdentityViewModel
    let android: AndroidVersionViewModel
    let logs: LogViewModel

    init() {
        cli = ProcessDsaCli(path: ProcessDsaCli.locate())
        service = DsaService(cli: cli)
        general = GeneralViewModel(service: service, text: appText)
        settings = SettingsViewModel(service: service, text: appText)
        settings.isAdministrator = Shell.isAdministrator
        gapps = GappsViewModel(service: service, text: appText)
        memory = MemoryViewModel(service: service, text: appText)
        install = InstallViewModel(service: service, text: appText)
        apps = AppsViewModel(service: AppsService(cli: cli), text: appText)
        widgets = WidgetsViewModel(service: service, text: appText, stream: cli)
        background = BackgroundViewModel(service: service, text: appText)
        identity = DeviceIdentityViewModel(service: service, text: appText)
        android = AndroidVersionViewModel(cli: cli, text: appText)
        logs = LogViewModel(stream: cli, service: service, text: appText)
        // The menu bar item shows Android's state while the app runs, so it is read every few
        // seconds; the General page reads it more often while it is shown.
        let polled = self.general
        Task { @MainActor in
            while true {
                await polled.poll()
                try? await Task.sleep(nanoseconds: 10_000_000_000)
            }
        }
    }
}

/// DSA.app's own program on macOS: the settings window in the style of System Settings, the
/// menu bar item with Android's state, and the Dock icon. It is a thin client of the DSA
/// service's local API, through the dsa command beside it.
@main
struct DsaSettingsApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate
    @StateObject private var model = AppModel()

    var body: some Scene {
        Window(T("WindowTitle"), id: SettingsWindow.id) {
            SettingsWindow()
                .environmentObject(model)
                .frame(minWidth: 720, minHeight: 480)
        }
        .defaultSize(width: 960, height: 680)
        .commands {
            CommandGroup(replacing: .appSettings) {
                OpenSettingsButton()
                    .keyboardShortcut(",", modifiers: .command)
            }
            CommandGroup(replacing: .newItem) {}
        }

        MenuBarExtra {
            MenuBarContent()
                .environmentObject(model)
                .environmentObject(model.general)
        } label: {
            Image(nsImage: MenuBarIcon.template)
        }
    }
}

final class AppDelegate: NSObject, NSApplicationDelegate {
    /// The menu bar item keeps the app running after its window closes.
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { false }
}

/// Opens the settings window, or brings it to the front.
struct OpenSettingsButton: View {
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        Button(T("MenuBar_OpenSettings")) {
            openWindow(id: SettingsWindow.id)
            NSApp.activate(ignoringOtherApps: true)
        }
    }
}

/// The menu bar item's menu: Android's state, starting and stopping it, the settings window,
/// and quitting the app.
struct MenuBarContent: View {
    @EnvironmentObject private var model: AppModel
    @EnvironmentObject private var general: GeneralViewModel

    var body: some View {
        Text(Format.fill(T("MenuBar_State"), [general.stateText]))
        Divider()
        Button(T("StartButton.Content")) { Task { await general.start() } }
            .disabled(!general.canStart)
        Button(T("StopButton.Content")) { Task { await general.stop() } }
            .disabled(!general.canStop)
        Divider()
        OpenSettingsButton()
        Divider()
        Button(T("MenuBar_Quit")) { NSApp.terminate(nil) }
            .keyboardShortcut("q")
    }
}
