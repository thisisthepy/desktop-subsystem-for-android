import AppKit
import DsaSettingsCore
import SwiftUI

/// General: Android's state, starting, stopping and freezing it, and the log folders.
struct GeneralPage: View {
    @ObservedObject var vm: GeneralViewModel
    @ObservedObject var settings: SettingsViewModel

    var body: some View {
        Form {
            BannerSection(shown: vm.error != nil, kind: .error, title: T("GeneralErrorBar.Title"), message: vm.error ?? "")
            Section {
                HStack {
                    RowLabel(title: T("GeneralStateCard.Header"), detail: T("GeneralStateCard.Description"))
                    Spacer()
                    if vm.isBusy {
                        ProgressView().controlSize(.small)
                    }
                    Text(vm.stateText).foregroundStyle(.secondary)
                    Button(T("StartButton.Content")) { Task { await vm.start() } }.disabled(!vm.canStart)
                    Button(T("StopButton.Content")) { Task { await vm.stop() } }.disabled(!vm.canStop)
                }
                if vm.hasProblem {
                    Banner(kind: .warning, title: T("ProblemBar.Title"), message: vm.problem)
                }
                if vm.hasLastReason {
                    Banner(kind: .info, title: T("LastReasonBar.Title"), message: vm.lastReason)
                }
                if vm.hasUpdateNotice {
                    Banner(kind: .info, title: T("UpdateNoticeBar.Title"), message: vm.updateNotice)
                }
            }
            Section {
                ButtonRow(title: T("FreezeNowCard.Header"), detail: T("FreezeNowCard.Description"),
                          action: T("FreezeNowButton.Content"), enabled: vm.canFreeze) {
                    Task { await vm.freeze() }
                }
            }
            Section(T("GeneralMoreHeader.Text")) {
                LinkRow(title: T("OpenLogsCard.Header"), detail: T("OpenLogsCard.Description")) {
                    Shell.open(folder: Shell.userLogFolder)
                }
                LinkRow(title: T("ServiceLogsCard.Header"), detail: T("ServiceLogsCard.Description")) {
                    Shell.open(folder: Shell.serviceLogFolder(settings.logDir))
                }
                LinkRow(title: T("RefreshCard.Header"), detail: T("RefreshCard.Description")) {
                    Task { await vm.refresh() }
                }
            }
        }
        .settingsPage()
        .task {
            await vm.refresh()
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 3_000_000_000)
                await vm.poll()
            }
        }
    }
}

/// One line of the memory page.
struct MemoryRowView: View {
    let row: MemoryRow

    var body: some View {
        LabeledContent {
            Text(row.size).monospacedDigit()
        } label: {
            RowLabel(title: row.label, detail: row.detail)
        }
    }
}

/// Memory: the limit, the VM's size, the host processes, and Android's breakdown.
struct MemoryPage: View {
    @ObservedObject var vm: MemoryViewModel
    @ObservedObject var settings: SettingsViewModel

    var body: some View {
        Form {
            BannerSection(shown: vm.error != nil, kind: .error, title: T("MemoryErrorBar.Title"), message: vm.error ?? "")
            BannerSection(shown: settings.error != nil, kind: .error, title: T("SettingsErrorBar.Title"), message: settings.error ?? "")
            Section {
                Picker(selection: Binding(get: { settings.memoryLimitMb },
                                          set: { mb in Task { await settings.setMemoryLimit(mb) } })) {
                    ForEach(settings.memoryLimitChoices, id: \.mb) { choice in
                        Text(choice.label).tag(choice.mb)
                    }
                } label: {
                    RowLabel(title: T("MemoryLimitCard.Header"), detail: T("MemoryLimitCard.Description"))
                }
                .disabled(!settings.canChange)
                if vm.hasVmSize {
                    ValueRow(title: T("MemoryVmSizeCard.Header"), detail: T("MemoryVmSizeCard.Description"), value: vm.vmSize)
                    if vm.guestReported {
                        LabeledContent(T("MemoryGuestUsedLabel.Text"), value: vm.guestUsed)
                    }
                    if vm.hasVmBeyondGuest {
                        LabeledContent(T("MemoryBeyondGuestLabel.Text"), value: vm.vmBeyondGuest)
                    }
                }
            }
            Section(T("MemoryHostHeader.Text")) {
                ValueRow(title: T("MemoryHostTotalCard.Header"), detail: T("MemoryHostTotalCard.Description"), value: vm.hostTotal)
                ForEach(Array(vm.hostRows.enumerated()), id: \.offset) { MemoryRowView(row: $0.element) }
                if vm.hostNotRunning {
                    Text(T("MemoryHostNone.Text")).foregroundStyle(.secondary)
                }
            }
            Section(T("MemoryGuestHeader.Text")) {
                if vm.guestReported {
                    VStack(alignment: .leading, spacing: 6) {
                        HStack {
                            Text(T("MemoryGuestTotalCard.Header"))
                            Spacer()
                            Text("\(vm.guestAvailable) \(T("MemoryAvailableOf.Text")) \(vm.guestTotal)").monospacedDigit()
                        }
                        ProgressView(value: vm.guestUsedFraction)
                        Text(vm.guestAge).font(.callout).foregroundStyle(.secondary)
                    }
                    ForEach(Array(vm.guestRows.enumerated()), id: \.offset) { MemoryRowView(row: $0.element) }
                } else {
                    Banner(kind: .info, title: T("MemoryGuestNoneBar.Title"), message: T("MemoryGuestNoneBar.Message"))
                }
            }
            if vm.kernelReported {
                Section(T("MemoryKernelHeader.Text")) {
                    ForEach(Array(vm.kernelRows.enumerated()), id: \.offset) { MemoryRowView(row: $0.element) }
                }
            }
            if vm.guestReported {
                Section(T("MemoryTopHeader.Text")) {
                    ForEach(Array(vm.guestProcessRows.enumerated()), id: \.offset) { MemoryRowView(row: $0.element) }
                }
            }
        }
        .settingsPage()
        .toolbar {
            Button { Task { await vm.refresh() } } label: { Label(T("MemoryRefreshButton.Content"), systemImage: "arrow.clockwise") }
        }
        .task {
            await settings.load()
            while !Task.isCancelled {
                await vm.refresh()
                try? await Task.sleep(nanoseconds: 5_000_000_000)
            }
        }
    }
}

/// Display and GPU: the render GPU, the main display's GPU, and restarting the window manager.
struct DisplayPage: View {
    @ObservedObject var vm: SettingsViewModel

    var body: some View {
        Form {
            BannerSection(shown: vm.error != nil, kind: .error, title: T("SettingsErrorBar.Title"), message: vm.error ?? "")
            Section {
                Picker(selection: Binding(get: { vm.selectedGpu },
                                          set: { value in Task { await vm.setRenderGpu(value) } })) {
                    ForEach(vm.gpuChoices, id: \.value) { choice in
                        Text(choice.label).tag(choice.value)
                    }
                } label: {
                    RowLabel(title: T("RenderGpuCard.Header"), detail: T("RenderGpuCard.Description"))
                }
                .disabled(!vm.canChange)
                ValueRow(title: T("DisplayGpuCard.Header"), detail: T("DisplayGpuCard.Description"), value: vm.displayGpuName)
                Banner(kind: .info, title: "", message: T("RenderGpuAppliesBar.Message"))
            }
            Section {
                ButtonRow(title: T("RestartWindowManagerCard.Header"), detail: T("RestartWindowManagerCard.Description"),
                          action: T("RestartWindowManagerButton.Content")) {
                    guard Shell.confirm(T("RestartWindowManagerTitle"), T("RestartWindowManagerBody"),
                                        T("RestartWindowManagerAction"), destructive: false) else {
                        return
                    }
                    Task {
                        if let problem = await Shell.restartWindowManager() {
                            Shell.tell(T("RestartWindowManagerFailed"), problem)
                        }
                    }
                }
            }
        }
        .settingsPage()
        .task { await vm.load() }
    }
}

/// Changes a setting that applies to every user by running dsa as an administrator, then
/// shows what the service keeps.
@MainActor
func changeAsAdministrator(_ settings: SettingsViewModel, _ args: [String]) async {
    if let problem = await Shell.runDsaElevated(args) {
        Shell.tell(T("SettingsErrorBar.Title"), problem)
    }
    await settings.load()
}

/// A switch that only an administrator changes: the switch itself for one, and for anyone
/// else the button that asks macOS for an administrator's password.
struct AdministratorSwitchRow: View {
    let title: String
    let detail: String
    let isOn: Bool
    let canChange: Bool
    let needsAdministrator: Bool
    let set: (Bool) async -> Void
    let asAdministrator: () -> Void

    var body: some View {
        HStack {
            RowLabel(title: title, detail: detail)
            Image(systemName: "lock.shield").foregroundStyle(.secondary).help(T("AdminShield.ToolTip"))
            Spacer()
            if needsAdministrator {
                Button(T("ChangeAsAdmin.Content"), action: asAdministrator)
            }
            Toggle("", isOn: Binding(get: { isOn }, set: { value in Task { await set(value) } }))
                .labelsHidden()
                .toggleStyle(.switch)
                .disabled(!canChange)
        }
    }
}

/// Updates: automatic updates of DSA and of the Android image, and installing now.
struct UpdatesPage: View {
    @ObservedObject var vm: SettingsViewModel
    @ObservedObject var general: GeneralViewModel
    let service: DsaService
    @State private var updating = false

    var body: some View {
        Form {
            BannerSection(shown: vm.error != nil, kind: .error, title: T("SettingsErrorBar.Title"), message: vm.error ?? "")
            BannerSection(shown: general.hasUpdateNotice, kind: .info, title: T("UpdateNoticeBar.Title"), message: general.updateNotice)
            BannerSection(shown: vm.needsAdministrator, kind: .warning, title: T("AdminBar.Title"), message: T("AdminBar.Message"))
            Section {
                AdministratorSwitchRow(
                    title: T("ProgramUpdatesCard.Header"), detail: T("ProgramUpdatesCard.Description"),
                    isOn: vm.programUpdates, canChange: vm.canChangeProgramUpdates,
                    needsAdministrator: vm.programUpdatesNeedAdministrator, set: { await vm.setProgramUpdates($0) }) {
                    Task { await changeAsAdministrator(vm, ["settings", "set", "program-updates", vm.programUpdates ? "off" : "on"]) }
                }
                AdministratorSwitchRow(
                    title: T("ImageUpdatesCard.Header"), detail: T("ImageUpdatesCard.Description"),
                    isOn: vm.imageUpdates, canChange: vm.canChangeImageUpdates,
                    needsAdministrator: vm.imageUpdatesNeedAdministrator, set: { await vm.setImageUpdates($0) }) {
                    Task { await changeAsAdministrator(vm, ["settings", "set", "image-updates", vm.imageUpdates ? "off" : "on"]) }
                }
                Text(T("UpdatesMachineNote.Text")).font(.callout).foregroundStyle(.secondary)
            }
            Section(T("UpdateNowHeader.Text")) {
                ButtonRow(title: T("UpdateNowCard.Header"), detail: T("UpdateNowCard.Description"),
                          action: T("UpdateNowButton.Content"), enabled: vm.canUpdateNow && !updating, busy: updating) {
                    guard Shell.confirm(T("UpdateNowTitle"), T("UpdateNowBody"), T("UpdateNowAction")) else {
                        return
                    }
                    updating = true
                    Task {
                        do {
                            Shell.tell(T("UpdateNowTitle"), try await service.updateNow())
                        } catch {
                            Shell.tell(T("UpdateNowFailed"), Format.problem(appText, error))
                        }
                        updating = false
                    }
                }
            }
        }
        .settingsPage()
        .task {
            await vm.load()
            await general.poll()
        }
    }
}

/// Android: the version that runs, and choosing one of the supported ones.
struct AndroidPage: View {
    @ObservedObject var vm: AndroidVersionViewModel
    @ObservedObject var settings: SettingsViewModel

    var body: some View {
        Form {
            BannerSection(shown: vm.error != nil, kind: .error, title: T("SettingsErrorBar.Title"), message: vm.error ?? "")
            BannerSection(shown: vm.isSwitching, kind: .info, title: T("AndroidSwitchingBar.Title"), message: vm.switchingText)
            BannerSection(shown: vm.hasNotice, kind: .info, title: T("UpdateNoticeBar.Title"), message: vm.notice)
            Section(T("AndroidRunningHeader.Text")) {
                HStack {
                    Text(vm.versionText).font(.title3.weight(.semibold))
                    Spacer()
                    if vm.isBusy {
                        ProgressView().controlSize(.small)
                    }
                    Text(vm.imageVersion).foregroundStyle(.secondary).textSelection(.enabled)
                }
                LabeledContent(T("AndroidBuildCard.Header"), value: vm.buildId)
                LabeledContent(T("AndroidPatchCard.Header"), value: vm.securityPatch)
            }
            Section(T("AndroidChooseHeader.Text")) {
                HStack {
                    Picker(selection: $vm.selectedChannel) {
                        ForEach(vm.choices, id: \.channel) { choice in
                            Text(choice.label).tag(Optional(choice.channel))
                        }
                    } label: {
                        HStack(alignment: .firstTextBaseline) {
                            RowLabel(title: T("AndroidChooseCard.Header"),
                                     detail: T("AndroidChooseCard.Description") + "\n" + vm.supportedText)
                            Image(systemName: "lock.shield").foregroundStyle(.secondary).help(T("AdminShield.ToolTip"))
                        }
                    }
                    Button(T("AndroidApplyButton.Content")) { apply() }.disabled(!vm.canApply)
                }
                Text(T("AndroidChooseNote.Text")).font(.callout).foregroundStyle(.secondary)
            }
        }
        .settingsPage()
        .task { await vm.load() }
    }

    private func apply() {
        guard let choice = vm.selected else {
            return
        }
        let move = vm.moveTo(choice)
        let confirmed: Bool
        switch move {
        case .down:
            // Moving down loses data, so the user reads what goes first.
            confirmed = Shell.confirm(T("AndroidDownConfirm.Title"), vm.downWarning(choice), T("AndroidDownConfirm.Action"))
        case .up:
            confirmed = Shell.confirm(T("AndroidUpConfirm.Title"), vm.upWarning(choice), T("AndroidApplyButton.Content"), destructive: false)
        case .same:
            confirmed = true
        }
        Task {
            guard confirmed else {
                await vm.load()
                return
            }
            let erase = move == .down
            // The image is shared by every user of this Mac, so the service takes the choice
            // from an administrator only; macOS asks for an administrator's password.
            if Shell.isAdministrator {
                await vm.use(choice, eraseData: erase)
                return
            }
            if let problem = await Shell.runDsaElevated(AndroidVersionViewModel.useArguments(choice, eraseData: erase)) {
                Shell.tell(T("SettingsErrorBar.Title"), problem)
            }
            await vm.load()
        }
    }
}

/// About: the versions, the licences, the source code, and the trademark notices.
struct AboutPage: View {
    @ObservedObject var vm: SettingsViewModel

    var body: some View {
        Form {
            Section {
                HStack(spacing: 14) {
                    Image(nsImage: NSApp.applicationIconImage).resizable().frame(width: 64, height: 64)
                    VStack(alignment: .leading, spacing: 2) {
                        Text(T("AboutProductCard.Header")).font(.title3.weight(.semibold))
                        Text(T("AboutProductCard.Description")).foregroundStyle(.secondary)
                    }
                    Spacer()
                    Text(Shell.appVersion).foregroundStyle(.secondary)
                }
            }
            Section {
                LabeledContent(T("AboutAppVersionCard.Header"), value: Shell.appVersion)
                LabeledContent(T("AboutServiceVersionCard.Header"), value: vm.serviceVersion)
                if vm.hasAndroidVersion {
                    ValueRow(title: T("AboutAndroidVersionCard.Header"), detail: T("AboutAndroidVersionCard.Description"),
                             value: vm.androidVersion)
                }
                LinkRow(title: T("AboutLicenseCard.Header"), detail: T("AboutLicenseCard.Description")) {
                    Shell.open(folder: Shell.licenseFolder)
                }
                LabeledContent(T("AboutSourceCard.Header")) {
                    Link("github.com/thisisthepy/desktop-subsystem-for-android",
                         destination: URL(string: "https://github.com/thisisthepy/desktop-subsystem-for-android")!)
                }
            }
            Section {
                Text(T("AboutIndependent.Text")).font(.callout).foregroundStyle(.secondary)
                Text(T("AboutTrademark.Text")).font(.callout).foregroundStyle(.secondary)
            }
        }
        .settingsPage()
        .task { await vm.load() }
    }
}
