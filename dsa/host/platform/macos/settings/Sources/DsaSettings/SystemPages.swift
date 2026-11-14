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
                        Note(text: vm.guestAge)
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

/// Updates: the Android image (the version that runs, the version it follows, its update with
/// the download's progress, and choosing a supported version), then DSA itself (automatic
/// updates, the release channel, installing now).
struct UpdatesPage: View {
    @ObservedObject var vm: SettingsViewModel
    @ObservedObject var android: AndroidVersionViewModel
    @ObservedObject var general: GeneralViewModel
    @ObservedObject var update: ProgramUpdateViewModel

    var body: some View {
        Form {
            BannerSection(shown: vm.error != nil, kind: .error, title: T("SettingsErrorBar.Title"), message: vm.error ?? "")
            BannerSection(shown: android.error != nil, kind: .error, title: T("SettingsErrorBar.Title"), message: android.error ?? "")
            BannerSection(shown: vm.needsAdministrator, kind: .warning, title: T("AdminBar.Title"), message: T("AdminBar.Message"))
            Section(T("UpdatesAndroidHeader.Text")) {
                HStack {
                    Text(android.versionText).font(.title3.weight(.semibold))
                    Spacer()
                    if android.isBusy {
                        ProgressView().controlSize(.small)
                    }
                    Text(android.imageVersion).foregroundStyle(.secondary).textSelection(.enabled)
                }
                LabeledContent(T("AndroidBuildCard.Header"), value: android.buildId)
                LabeledContent(T("AndroidPatchCard.Header"), value: android.securityPatch)
                LabeledContent(T("AndroidFollowsCard.Header"), value: android.channelText)
                HStack(alignment: .firstTextBaseline) {
                    VStack(alignment: .leading, spacing: 4) {
                        HStack(alignment: .firstTextBaseline) {
                            RowLabel(title: T("AndroidUpdateCard.Header"), detail: android.updateStatusText)
                            Image(systemName: "lock.shield").foregroundStyle(.secondary).help(T("AdminShield.ToolTip"))
                        }
                        if let percent = android.downloadPercent {
                            ProgressView(value: Double(percent), total: 100)
                        }
                    }
                    Spacer()
                    // Shown while a downloaded image waits for this user's Android to restart.
                    if android.isRestartPending {
                        Button(T("AndroidRestartButton.Content")) { restartNow() }
                            .buttonStyle(.borderedProminent)
                            .disabled(!android.canRestartNow)
                    }
                    Button(T("AndroidCheckButton.Content")) { checkNow() }.disabled(!android.canCheckNow)
                }
                SwitchRow(title: T("ImageAutoRestartCard.Header"), detail: T("ImageAutoRestartCard.Description"),
                          isOn: vm.imageAutoRestart, enabled: vm.canChange) { await vm.setImageAutoRestart($0) }
                AdministratorSwitchRow(
                    title: T("ImageUpdatesCard.Header"), detail: T("ImageUpdatesCard.Description"),
                    isOn: vm.imageUpdates, canChange: vm.canChangeImageUpdates,
                    needsAdministrator: vm.imageUpdatesNeedAdministrator, set: { await vm.setImageUpdates($0) }) {
                    Task { await changeAsAdministrator(vm, ["settings", "set", "image-updates", vm.imageUpdates ? "off" : "on"]) }
                }
                HStack {
                    Picker(selection: $android.selectedChannel) {
                        ForEach(android.choices, id: \.channel) { choice in
                            Text(choice.label).tag(Optional(choice.channel))
                        }
                    } label: {
                        HStack(alignment: .firstTextBaseline) {
                            RowLabel(title: T("AndroidChooseCard.Header"),
                                     detail: T("AndroidChooseCard.Description") + "\n" + android.supportedText)
                            Image(systemName: "lock.shield").foregroundStyle(.secondary).help(T("AdminShield.ToolTip"))
                        }
                    }
                    Button(T("AndroidApplyButton.Content")) { apply() }.disabled(!android.canApply)
                }
                Note(text: T("AndroidChooseNote.Text"))
            }
            Section(T("UpdatesProgramHeader.Text")) {
                if general.hasUpdateNotice {
                    Banner(kind: .info, title: T("UpdateNoticeBar.Title"), message: general.updateNotice)
                }
                AdministratorSwitchRow(
                    title: T("ProgramUpdatesCard.Header"), detail: T("ProgramUpdatesCard.Description"),
                    isOn: vm.programUpdates, canChange: vm.canChangeProgramUpdates,
                    needsAdministrator: vm.programUpdatesNeedAdministrator, set: { await vm.setProgramUpdates($0) }) {
                    Task { await changeAsAdministrator(vm, ["settings", "set", "program-updates", vm.programUpdates ? "off" : "on"]) }
                }
                AdministratorSwitchRow(
                    title: T("PrereleaseUpdatesCard.Header"), detail: T("PrereleaseUpdatesCard.Description"),
                    isOn: vm.prereleaseUpdates, canChange: vm.canChangePrereleaseUpdates,
                    needsAdministrator: vm.prereleaseUpdatesNeedAdministrator, set: { await vm.setPrereleaseUpdates($0) }) {
                    Task { await changeAsAdministrator(vm, ["settings", "set", "prerelease-updates", vm.prereleaseUpdates ? "off" : "on"]) }
                }
                // An update of DSA while it runs, across the service's restart, and how it ended.
                if update.inProgress {
                    VStack(alignment: .leading, spacing: 4) {
                        Text(update.phaseText).font(.headline)
                        if update.hasVersion {
                            Text(update.versionText).foregroundStyle(.secondary)
                        }
                        Text(update.keepOpenText).foregroundStyle(.secondary)
                        ProgressView().progressViewStyle(.linear)
                            .accessibilityLabel(T("ProgramUpdateCard.Header"))
                    }
                }
                if update.isApplied {
                    Banner(kind: .success, title: T("ProgramUpdateDone.Title"), message: update.appliedText)
                }
                if update.isFailed {
                    Banner(kind: .error, title: T("UpdateNowFailed"), message: update.failedText)
                }
                ButtonRow(title: T("UpdateNowCard.Header"), detail: T("UpdateNowCard.Description"),
                          action: T("UpdateNowButton.Content"), enabled: vm.canUpdateNow && update.canStartUpdate,
                          busy: update.inProgress) {
                    guard Shell.confirm(T("UpdateNowTitle"), T("UpdateNowBody"), T("UpdateNowAction")) else {
                        return
                    }
                    Task {
                        let answer = await update.updateNow()
                        if let error = update.error {
                            Shell.tell(T("UpdateNowFailed"), error)
                        } else if let answer, answer.installing == nil {
                            // Nothing newer: the service says why.
                            Shell.tell(T("UpdateNowTitle"), answer.detail)
                        }
                    }
                }
                Note(text: T("UpdatesMachineNote.Text"))
            }
        }
        .settingsPage()
        .task {
            await update.poll()
            await vm.load()
            await android.load()
            await general.poll()
            // A download moves on by itself, so the version is read again while the page is
            // shown; the task ends when the page goes away.
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 3_000_000_000)
                await android.refresh()
            }
        }
        .task {
            // An update of DSA is followed more closely, across the service's restart; once the
            // new service answers, the page reads what it says now, its version among it.
            update.cameBack = {
                Task {
                    await vm.load()
                    await android.load()
                    await general.poll()
                }
            }
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 1_500_000_000)
                await update.poll()
            }
        }
    }

    /// Asks the service to look for an Android image now. The image is shared by every user of
    /// this Mac, so an administrator asks; macOS asks for an administrator's password.
    private func checkNow() {
        Task {
            if Shell.isAdministrator {
                await android.checkNow()
                return
            }
            if let problem = await Shell.runDsaElevated(android.checkArguments) {
                Shell.tell(T("SettingsErrorBar.Title"), problem)
            }
            await android.load()
        }
    }

    /// Restarts Android now into the downloaded image, after the user agrees that its app
    /// windows close. The restart is the user's own Android's, so no administrator is asked.
    private func restartNow() {
        guard let version = android.restartPending,
              Shell.confirm(T("AndroidRestartConfirm.Title"), appText.format("AndroidRestartConfirm.Body", version),
                            T("AndroidRestartButton.Content"), destructive: false) else {
            return
        }
        Task { await android.restartNow() }
    }

    private func apply() {
        guard let choice = android.selected else {
            return
        }
        let move = android.moveTo(choice)
        let confirmed: Bool
        switch move {
        case .down:
            // Moving down loses data, so the user reads what goes first.
            confirmed = Shell.confirm(T("AndroidDownConfirm.Title"), android.downWarning(choice), T("AndroidDownConfirm.Action"))
        case .up:
            confirmed = Shell.confirm(T("AndroidUpConfirm.Title"), android.upWarning(choice), T("AndroidApplyButton.Content"), destructive: false)
        case .same:
            confirmed = true
        }
        Task {
            guard confirmed else {
                await android.load()
                return
            }
            let erase = move == .down
            // The image is shared by every user of this Mac, so the service takes the choice
            // from an administrator only; macOS asks for an administrator's password.
            if Shell.isAdministrator {
                await android.use(choice, eraseData: erase)
                return
            }
            if let problem = await Shell.runDsaElevated(AndroidVersionViewModel.useArguments(choice, eraseData: erase)) {
                Shell.tell(T("SettingsErrorBar.Title"), problem)
            }
            await android.load()
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
                Note(text: T("AboutIndependent.Text"))
                Note(text: T("AboutTrademark.Text"))
            }
        }
        .settingsPage()
        .task { await vm.load() }
    }
}
