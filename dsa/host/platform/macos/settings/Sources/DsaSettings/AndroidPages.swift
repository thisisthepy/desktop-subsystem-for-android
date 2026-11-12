import AppKit
import DsaSettingsCore
import SwiftUI

/// Apps: every app installed in Android, to find, open, see in Android's own settings,
/// uninstall, or change how its windows size; and Android's own list of apps.
struct AppsPage: View {
    @ObservedObject var vm: AppsViewModel

    var body: some View {
        Form {
            Section {
                Text(T("AppsIntro.Text")).font(.callout).foregroundStyle(.secondary)
                Text(T("AppsRootNote.Text")).font(.callout).foregroundStyle(.secondary)
            }
            BannerSection(shown: vm.error != nil, kind: .error, title: T("AppsErrorBar.Title"), message: vm.error ?? "")
            BannerSection(shown: vm.message != nil, kind: .success, title: T("AppsDoneBar.Title"), message: vm.message ?? "")
            // Each app's root level is chosen below once root access is on for this Mac.
            BannerSection(shown: vm.rootUnavailable, kind: .info, title: T("AppsRootOffBar.Title"),
                          message: T("AppsRootOffBar.Message"))
            Section {
                LinkRow(title: T("AppsManageCard.Title"), detail: T("AppsManageCard.Description"), enabled: !vm.isBusy) {
                    Task { await vm.manage() }
                }
            }
            Section {
                HStack {
                    TextField(T("AppsSearch.PlaceholderText"), text: $vm.search)
                        .textFieldStyle(.roundedBorder)
                    Text(vm.count).foregroundStyle(.secondary).monospacedDigit()
                }
                if vm.hasNoApps {
                    Text(T("AppsNone.Text")).foregroundStyle(.secondary)
                }
                if vm.hasNoMatches {
                    Text(T("AppsNoMatch.Text")).foregroundStyle(.secondary)
                }
                ForEach(vm.rows) { row in
                    AppRowView(vm: vm, row: row)
                }
            } header: {
                HStack {
                    Text(T("AppsInstalledHeader.Text"))
                    if vm.isBusy {
                        ProgressView().controlSize(.small)
                    }
                }
            }
        }
        .settingsPage()
        .toolbar {
            Button { Task { await vm.refresh() } } label: { Label(T("AppsRefreshButton.Content"), systemImage: "arrow.clockwise") }
                .disabled(vm.isBusy)
        }
        .task {
            await vm.refresh()
            await vm.loadWindowSettings()
        }
    }
}

/// One app: its icon, name and details, its window settings, and what can be done with it.
struct AppRowView: View {
    @ObservedObject var vm: AppsViewModel
    let row: AppRow

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 10) {
                PngImage(data: row.icon)
                RowLabel(title: row.label, detail: row.detail)
                Spacer()
                Text(row.size).foregroundStyle(.secondary).monospacedDigit()
            }
            HStack {
                Toggle(T("AppsResizable.Content"), isOn: Binding(
                    get: { row.resizable },
                    set: { value in Task { await vm.setResizable(row.package, value) } }))
                    .toggleStyle(.checkbox)
                    .help(T("AppsResizable.ToolTip"))
                    .disabled(vm.isBusy)
                Picker(T("AppsStartSize.ToolTip"), selection: Binding(
                    get: { row.startSizeIndex },
                    set: { index in Task { await vm.setStartSize(row.package, index) } })) {
                    if row.startSizeIndex < 0 {
                        Text(vm.windowOf(row.package).startSizeArgument).tag(-1)
                    }
                    ForEach(Array(vm.startSizeChoices.enumerated()), id: \.offset) { item in
                        Text(item.element.label).tag(item.offset)
                    }
                }
                .labelsHidden()
                .help(T("AppsStartSize.ToolTip"))
                .frame(maxWidth: 260)
                .disabled(vm.isBusy)
                // How the app's windows follow a monitor of another scale: the app redraws, or
                // its window is scaled. Auto and system never restart the app.
                Picker(T("AppsDpiScaling.Name"), selection: Binding(
                    get: { row.dpiScalingIndex },
                    set: { index in Task { await vm.setDpiScaling(row.package, index) } })) {
                    ForEach(Array(vm.dpiScalingChoices.enumerated()), id: \.offset) { item in
                        Text(item.element.label).tag(item.offset)
                    }
                }
                .labelsHidden()
                .help(row.dpiScalingTip ?? T("AppsDpiScaling.ToolTip"))
                .accessibilityLabel(T("AppsDpiScaling.Name"))
                .frame(maxWidth: 300)
                .disabled(vm.isBusy)
                Spacer()
                Button(T("AppsOpenButton.Content")) { Task { await vm.open(row.package) } }
                Button(T("AppsInfoButton.Content")) { Task { await vm.info(row.package) } }
                Button(T("AppsDesktopShortcutButton.Content")) { Task { await vm.desktopShortcut(row.package) } }
                    .help(T("AppsDesktopShortcutButton.ToolTip"))
                Button(T("AppsUninstallButton.Content")) { uninstall() }
                    .disabled(!row.canUninstall || vm.isBusy)
                    .help(row.uninstallTip ?? "")
            }
            .disabled(vm.isBusy)
            // Whether the app runs its ARM code through the translator, and why not when it cannot.
            HStack {
                Toggle(T("AppsRunAsArm.Content"), isOn: Binding(
                    get: { row.runAsArm },
                    set: { value in Task { await vm.setRunAsArm(row.package, value, confirmErase: confirmErase) } }))
                    .toggleStyle(.switch)
                    .disabled(!row.canRunAsArm || vm.isBusy)
                    .help(row.runAsArmTip ?? "")
                if let tip = row.runAsArmTip {
                    Text(tip).font(.callout).foregroundStyle(.secondary)
                }
                // How far the app may use root: not at all, asking an administrator each time,
                // or without asking.
                Picker(T("AppsRoot.Name"), selection: Binding(
                    get: { row.rootIndex },
                    set: { index in Task { await vm.setRoot(row.package, index) } })) {
                    if row.rootIndex < 0 {
                        Text(T("AppsRoot.Name")).tag(-1)
                    }
                    ForEach(Array(vm.rootChoices.enumerated()), id: \.offset) { item in
                        Text(item.element.label).tag(item.offset)
                    }
                }
                .labelsHidden()
                .frame(maxWidth: 260)
                .disabled(!row.canChangeRoot || vm.isBusy)
                .help(row.rootTip ?? T("AppsRootNote.Text"))
                Spacer()
            }
            // Apps that share a user ID: which, and that root is one setting for them.
            if let note = row.rootGroupNote {
                Text(note).font(.callout).foregroundStyle(.secondary)
            }
        }
        .padding(.vertical, 2)
    }

    /// Asks whether to erase the app's data, which Android cannot keep when the app's ABI changes.
    @MainActor
    private func confirmErase(_ label: String) async -> Bool {
        Shell.confirm(Format.fill(T("AppsRunAsArmEraseTitle"), [label]),
                      Format.fill(T("AppsRunAsArmEraseBody"), [label]),
                      T("AppsRunAsArmEraseButton"))
    }

    private func uninstall() {
        let label = vm.labelOf(row.package)
        guard Shell.confirm(Format.fill(T("AppsUninstallConfirmTitle"), [label]),
                            Format.fill(T("AppsUninstallConfirmBody"), [label]),
                            T("AppsUninstallButton.Content")) else {
            return
        }
        Task { await vm.uninstall(row.package) }
    }
}

/// Running in the background: the apps that keep running with no window, and whether a stop
/// from the menu bar asks first.
struct BackgroundPage: View {
    @ObservedObject var vm: BackgroundViewModel

    var body: some View {
        Form {
            Section {
                Text(T("BackgroundIntro.Text")).font(.callout).foregroundStyle(.secondary)
            }
            BannerSection(shown: vm.error != nil, kind: .error, title: T("BackgroundErrorBar.Title"), message: vm.error ?? "")
            BannerSection(shown: vm.message != nil, kind: .success, title: T("BackgroundDoneBar.Title"), message: vm.message ?? "")
            Section {
                if vm.hasNone {
                    Text(T("BackgroundNone.Text")).foregroundStyle(.secondary)
                }
                ForEach(vm.apps) { app in
                    HStack(spacing: 10) {
                        PngImage(data: app.icon)
                        RowLabel(title: app.label, detail: app.detail)
                        Spacer()
                        Button(T("BackgroundOpenButton.Content")) { Task { await vm.open(app.package) } }
                        Button(T("BackgroundStopButton.Content")) { Task { await vm.stop(app.package) } }
                    }
                    .disabled(vm.isBusy)
                }
            } header: {
                HStack {
                    Text(T("BackgroundAppsHeader.Text"))
                    if vm.isBusy {
                        ProgressView().controlSize(.small)
                    }
                }
            }
            Section(T("BackgroundStoppingHeader.Text")) {
                SwitchRow(title: T("BackgroundAskCard.Header"), detail: T("BackgroundAskCard.Description"),
                          isOn: vm.askBeforeStop, enabled: !vm.isBusy, set: { await vm.setAskBeforeStop($0) })
            }
        }
        .settingsPage()
        .toolbar {
            Button { Task { await vm.refresh() } } label: { Label(T("BackgroundRefreshButton.Content"), systemImage: "arrow.clockwise") }
                .disabled(vm.isBusy)
        }
        .task { await vm.refresh() }
    }
}

/// Widgets: the widgets placed on the desktop and the ones the apps offer. While the page is
/// shown it watches the placed widgets, so one placed or removed elsewhere shows at once.
struct WidgetsPage: View {
    @ObservedObject var vm: WidgetsViewModel

    var body: some View {
        Form {
            Section {
                Text(T("WidgetsIntro.Text")).font(.callout).foregroundStyle(.secondary)
            }
            BannerSection(shown: vm.error != nil, kind: .error, title: T("WidgetsErrorBar.Title"), message: vm.error ?? "")
            BannerSection(shown: vm.message != nil, kind: .success, title: T("WidgetsDoneBar.Title"), message: vm.message ?? "")
            Section {
                if vm.hasNoPlaced {
                    Text(T("WidgetsPlacedNone.Text")).foregroundStyle(.secondary)
                }
                ForEach(vm.placed) { widget in
                    HStack {
                        RowLabel(title: widget.label, detail: widget.app + "\n" + widget.detail)
                        Spacer()
                        Button(T("WidgetsRemoveButton.Content")) { Task { await vm.remove(widget.id) } }
                            .disabled(vm.isBusy)
                    }
                }
            } header: {
                HStack {
                    Text(T("WidgetsPlacedHeader.Text"))
                    if vm.isBusy {
                        ProgressView().controlSize(.small)
                    }
                }
            }
            Section(T("WidgetsGalleryHeader.Text")) {
                if vm.hasNoOffers {
                    Text(T("WidgetsGalleryNone.Text")).foregroundStyle(.secondary)
                }
                ForEach(vm.offers) { offer in
                    HStack(spacing: 10) {
                        PngImage(data: offer.preview, size: 56, placeholder: "rectangle.3.group")
                        RowLabel(title: offer.label, detail: offer.app + "\n" + offer.detail)
                        Spacer()
                        Button(T("WidgetsAddButton.Content")) { Task { await vm.add(offer.provider) } }
                            .disabled(vm.isBusy)
                    }
                }
            }
        }
        .settingsPage()
        .toolbar {
            Button { Task { await vm.refresh() } } label: { Label(T("WidgetsRefreshButton.Content"), systemImage: "arrow.clockwise") }
                .disabled(vm.isBusy)
        }
        .task {
            await vm.refresh()
            // The watch runs dsa as a child process; leaving the page cancels it, which ends dsa.
            await vm.watch(retry: 5)
        }
    }
}

/// One value of the device identity: its box, with Android's own value as the hint.
struct IdentityFieldRow: View {
    @ObservedObject var vm: DeviceIdentityViewModel
    let key: String

    var body: some View {
        let field = vm.field(key)
        let problem = vm.problem(field)
        VStack(alignment: .leading, spacing: 4) {
            LabeledContent {
                TextField(field.placeholder, text: Binding(get: { vm.field(key).value }, set: { vm.edit(key, $0) }))
                    .textFieldStyle(.roundedBorder)
                    .frame(maxWidth: 320)
                    .disabled(vm.isBusy)
            } label: {
                RowLabel(title: vm.label(field), detail: vm.detail(field))
            }
            Text(vm.originText(field.origin)).font(.caption).foregroundStyle(.secondary)
            if !problem.isEmpty {
                Text(problem).font(.callout).foregroundStyle(.red)
            }
        }
    }
}

/// Device identity: this Mac as its firmware and system report it (its name, maker and model,
/// then its specifications, which cannot be changed), and then the names and build
/// fingerprint Android reports to its apps, which can.
struct DeviceIdentityPage: View {
    @ObservedObject var vm: DeviceIdentityViewModel

    var body: some View {
        Form {
            // This Mac, as System Settings' About shows it: the name large, the maker and model
            // under it, then label and value rows.
            Section {
                VStack(alignment: .leading, spacing: 2) {
                    Text(vm.computerName).font(.title.weight(.semibold)).textSelection(.enabled)
                    Text(vm.computerMakeModel).foregroundStyle(.secondary).textSelection(.enabled)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.vertical, 4)
            }
            Section {
                ForEach(DeviceIdentityViewModel.aboutKeys, id: \.self) { key in
                    LabeledContent(vm.aboutLabel(key)) {
                        Text(vm.aboutValue(key)).textSelection(.enabled)
                    }
                }
            } header: {
                HStack {
                    Text(T("IdentitySpecsCard.Header"))
                    Spacer()
                    Button(T("IdentityCopyButton.Content")) {
                        NSPasteboard.general.clearContents()
                        NSPasteboard.general.setString(vm.aboutText, forType: .string)
                    }
                    .buttonStyle(.borderless)
                }
            } footer: {
                Text(T("IdentityComputerNote.Text")).font(.callout).foregroundStyle(.secondary)
            }
            Section(T("IdentityAndroidHeader.Text")) {
                Text(T("IdentityIntro.Text")).font(.callout).foregroundStyle(.secondary)
                Banner(kind: .warning, title: T("IdentityWarning.Title"), message: T("IdentityWarning.Message"))
            }
            BannerSection(shown: vm.error != nil, kind: .error, title: T("SettingsErrorBar.Title"), message: vm.error ?? "")
            BannerSection(shown: vm.message != nil, kind: .success, title: T("IdentityDoneBar.Title"), message: vm.message ?? "")
            Section {
                HStack {
                    if vm.isBusy {
                        ProgressView().controlSize(.small)
                    }
                    Spacer()
                    if vm.needsRestart {
                        Button(T("IdentityRestartButton.Content")) { Task { await vm.restart() } }.disabled(!vm.canRestart)
                    }
                    Button(T("IdentityResetButton.Content")) {
                        guard Shell.confirm(T("IdentityResetConfirmTitle"), T("IdentityResetConfirmBody"), T("IdentityResetButton.Content")) else {
                            return
                        }
                        Task { await vm.reset() }
                    }
                    .disabled(!vm.canReset)
                    Button(T("IdentitySaveButton.Content")) { Task { await vm.save() } }
                        .keyboardShortcut(.defaultAction)
                        .disabled(!vm.canSave)
                }
            }
            Section(T("IdentityNamesHeader.Text")) {
                ForEach(vm.names) { IdentityFieldRow(vm: vm, key: $0.key) }
            }
            Section(T("IdentityFingerprintHeader.Text")) {
                LabeledContent(T("IdentityPreview.Text")) {
                    Text(vm.fingerprintPreview).font(.system(.body, design: .monospaced)).textSelection(.enabled)
                }
                ForEach(vm.fingerprintParts) { IdentityFieldRow(vm: vm, key: $0.key) }
                IdentityFieldRow(vm: vm, key: vm.wholeFingerprint.key)
            }
        }
        .settingsPage()
        .task { await vm.load() }
    }
}

/// One root certificate the service found added to this Mac: who it is for, who signed it,
/// where it was found and the store it is in. VoiceOver reads the row as one item.
struct ImportedCertificateRow: View {
    let certificate: ImportedCertificate
    let issuer: String
    let source: String

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(certificate.subject).textSelection(.enabled)
            Text(issuer).font(.callout).foregroundStyle(.secondary).textSelection(.enabled)
            Text(source).font(.callout).foregroundStyle(.secondary)
            if !certificate.location.isEmpty {
                Text(certificate.location).font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
            }
        }
        .fixedSize(horizontal: false, vertical: true)
        .accessibilityElement(children: .combine)
    }
}

/// Network: how Android reaches the network now, the advanced networking switch, the root
/// certificates Android trusts from this Mac, and developer mode, through which adb on this
/// Mac reaches Android.
struct NetworkPage: View {
    @ObservedObject var vm: SettingsViewModel
    @State private var adbPort = ""

    var body: some View {
        Form {
            BannerSection(shown: vm.error != nil, kind: .error, title: T("SettingsErrorBar.Title"), message: vm.error ?? "")
            Section {
                HStack {
                    RowLabel(title: T("NetworkStatusCard.Header"), detail: vm.networkModeText)
                    Spacer()
                    Button(T("NetworkRefreshButton.Content")) { Task { await vm.refreshNetwork() } }
                }
                if vm.hasNetworkProblem {
                    Banner(kind: .warning, title: T("NetworkProblemBar.Title"), message: vm.networkProblem)
                }
                if vm.hasNetworkDetails {
                    LabeledContent(T("NetworkAddressLabel.Text")) { Text(vm.networkAddresses).textSelection(.enabled) }
                    LabeledContent(T("NetworkDnsLabel.Text")) { Text(vm.networkDns).textSelection(.enabled) }
                    LabeledContent(T("NetworkGatewayLabel.Text")) { Text(vm.networkGateway).textSelection(.enabled) }
                }
            }
            Section(T("NetworkAdvancedHeader.Text")) {
                SwitchRow(title: T("AdvancedNetworkingCard.Header"), detail: T("AdvancedNetworkingCard.Description"),
                          isOn: vm.advancedNetworking, enabled: vm.canChange, set: { await vm.setAdvancedNetworking($0) })
                if vm.networkRestartNeeded {
                    Banner(kind: .info, title: T("NetworkRestartBar.Title"), message: T("NetworkRestartBar.Message"))
                }
            }
            Section(T("HostCertificatesHeader.Text")) {
                SwitchRow(title: T("HostCertificatesCard.Header"), detail: T("HostCertificatesCard.Description"),
                          isOn: vm.hostCertificates, enabled: vm.canChange, set: { await vm.setHostCertificates($0) })
                if vm.hasNoImportedCertificates {
                    Text(T("HostCertificatesEmpty.Text")).font(.callout).foregroundStyle(.secondary)
                } else {
                    if !vm.hostCertificates {
                        Text(T("HostCertificatesOff.Text")).font(.callout).foregroundStyle(.secondary)
                    }
                    ForEach(Array(vm.importedCertificates.enumerated()), id: \.offset) { item in
                        ImportedCertificateRow(certificate: item.element, issuer: vm.certificateIssuerText(item.element),
                                               source: vm.certificateSourceText(item.element.source))
                    }
                }
            }
            Section(T("DeveloperHeader.Text")) {
                SwitchRow(title: T("DeveloperModeCard.Header"), detail: T("DeveloperModeCard.Description"),
                          isOn: vm.developerMode, enabled: vm.canChange, set: { await vm.setDeveloperMode($0) })
                SwitchRow(title: T("AdbAutoConnectCard.Header"), detail: T("AdbAutoConnectCard.Description"),
                          isOn: vm.adbAutoConnect, enabled: vm.canChange, set: { await vm.setAdbAutoConnect($0) })
                Text(vm.adbAutoConnectStateText).font(.callout).foregroundStyle(.secondary)
                LabeledContent {
                    HStack {
                        Text(vm.adbConnect).font(.system(.body, design: .monospaced)).textSelection(.enabled)
                        CopyButton(text: vm.adbConnect)
                    }
                } label: {
                    RowLabel(title: T("AdbCard.Header"), detail: T("AdbCard.Description"))
                }
                LabeledContent(T("AdbPortLabel.Text")) {
                    TextField("", text: $adbPort)
                        .textFieldStyle(.roundedBorder)
                        .frame(width: 90)
                        .onSubmit { Task { await vm.setAdbPort(adbPort) } }
                        .disabled(!vm.canChange)
                }
            }
        }
        .settingsPage()
        .task {
            await vm.load()
            adbPort = String(vm.adbPort)
            await vm.refreshNetwork()
            // Where auto-connect stands changes by itself, so it is read again while the page
            // is shown; the task ends when the page goes away.
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 3_000_000_000)
                await vm.pollAdbAutoConnect()
            }
        }
        .onChange(of: vm.adbPort) { port in adbPort = String(port) }
    }
}

/// Advanced: the clipboard, file sharing, links, freezing, the app data backup, resetting the
/// data, ARM apps, the traces, the logs and the command line.
struct AdvancedPage: View {
    @ObservedObject var vm: SettingsViewModel
    @ObservedObject var gapps: GappsViewModel
    @ObservedObject var widevine: WidevineViewModel
    @ObservedObject var backup: BackupViewModel

    var body: some View {
        Form {
            BannerSection(shown: vm.error != nil, kind: .error, title: T("SettingsErrorBar.Title"), message: vm.error ?? "")
            Section {
                SwitchRow(title: T("ClipboardCard.Header"), detail: T("ClipboardCard.Description"),
                          isOn: vm.clipboard, enabled: vm.canChange, set: { await vm.setClipboard($0) })
            }
            Section(T("FilesHeader.Text")) {
                SwitchRow(title: T("FileSharingCard.Header"), detail: T("FileSharingCard.Description"),
                          isOn: vm.fileSharing, enabled: vm.canChange, set: { await vm.setFileSharing($0) })
                VStack(alignment: .leading, spacing: 6) {
                    RowLabel(title: T("SharedFoldersCard.Header"), detail: T("SharedFoldersCard.Description"))
                    HStack(spacing: 14) {
                        ForEach(SettingsInfo.allFolders, id: \.self) { folder in
                            Toggle(T("Folder_\(folder)"), isOn: Binding(
                                get: { vm.isFolderShared(folder) },
                                set: { shared in Task { await vm.setFolderShared(folder, shared) } }))
                                .toggleStyle(.checkbox)
                        }
                    }
                    .disabled(!vm.canChooseFolders)
                }
                LinkRow(title: T("AndroidFilesCard.Header"), detail: T("AndroidFilesCard.Description")) {
                    Task { await vm.openAndroidFiles() }
                }
                // A row of its own, so the text wraps inside the form's width like the others.
                RowLabel(title: T("FilesDropCard.Header"), detail: T("FilesNote.Text"))
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            Section {
                SwitchRow(title: T("FreezeCard.Header"), detail: T("FreezeCard.Description"),
                          isOn: vm.freeze, enabled: vm.canChange, set: { await vm.setFreeze($0) })
                SwitchRow(title: T("LinksCard.Header"), detail: T("LinksCard.Description"),
                          isOn: vm.linksInAndroid, enabled: vm.canChange, set: { await vm.setLinksInAndroid($0) })
            }
            BackupSection(vm: backup)
            Section(T("ResetDataHeader.Text")) {
                if vm.resetDone {
                    Banner(kind: .success, title: T("ResetDoneBar.Title"), message: T("ResetDoneBar.Message"))
                }
                ButtonRow(title: T("ResetDataCard.Header"), detail: T("ResetDataCard.Description"),
                          action: T("ResetDataButton.Content"), enabled: vm.canResetData, busy: vm.isResetting) {
                    guard Shell.confirm(T("ResetDataConfirm.Title"), vm.resetWarning, T("ResetDataConfirm.Action")) else {
                        return
                    }
                    Task { await vm.resetData() }
                }
            }
            Section(T("ArmHeader.Text")) {
                SwitchRow(title: T("ArmCard.Header"), detail: vm.armStatus + "\n" + vm.armAbis + "\n" + T("ArmCard.Description"),
                          isOn: vm.armTranslation, enabled: vm.canChange, set: { await vm.setArmTranslation($0) })
                if vm.armRestartNeeded {
                    Banner(kind: .info, title: T("ArmRestartBar.Title"), message: T("ArmRestartBar.Message"))
                }
                if vm.hasArmDefault {
                    ButtonRow(title: T("ArmDefaultCard.Header"), detail: vm.armDefaultDescription,
                              action: T("ArmDefaultButton.Content"), enabled: vm.canUseDefaultArmTranslator) {
                        Task { await vm.useDefaultArmTranslator() }
                    }
                }
                if vm.showsArmRemove {
                    ButtonRow(title: T("ArmRemoveCard.Header"), detail: T("ArmRemoveCard.Description"),
                              action: T("ArmRemoveButton.Content"), enabled: vm.canRemoveArmTranslation) {
                        Task { await vm.removeArmTranslation() }
                    }
                }
                if vm.showsGoogleArm {
                    ButtonRow(title: T("ArmGoogleCard.Header"),
                              detail: vm.isFetchingGoogleArm ? T("ArmGoogleFetching.Text") : T("ArmGoogleCard.Description"),
                              action: T("ArmGoogleButton.Content"), enabled: vm.canFetchGoogleArm, busy: vm.isFetchingGoogleArm) {
                        guard Shell.confirm(T("ArmGoogleConfirm.Title"), vm.googleArmNotice, T("ArmGoogleConfirm.Action"),
                                            destructive: false) else {
                            return
                        }
                        Task { await vm.fetchGoogleArmTranslator() }
                    }
                    if let terms = URL(string: vm.googleArmTermsUrl) {
                        Link(T("ArmGoogleTermsLink.Content"), destination: terms).font(.callout)
                    }
                }
            }
            GappsSection(vm: gapps)
            WidevineSection(vm: widevine)
            Section(T("RootHeader.Text")) {
                AdministratorSwitchRow(
                    title: T("RootElevationCard.Header"), detail: T("RootElevationCard.Description"),
                    isOn: vm.rootElevation, canChange: vm.canChangeRootElevation,
                    needsAdministrator: vm.rootElevationNeedsAdministrator, set: { await vm.setRootElevation($0) }) {
                    Task { await changeAsAdministrator(vm, ["settings", "set", "root-elevation", vm.rootElevation ? "off" : "on"]) }
                }
                // Why the switch does not move for this user.
                if vm.rootElevationNeedsAdministrator {
                    Text(T("RootElevationAdminReason.Text")).font(.callout).foregroundStyle(.secondary)
                }
                if vm.rootElevationRestartNeeded {
                    Banner(kind: .info, title: T("RootElevationRestartBar.Title"), message: T("RootElevationRestartBar.Message"))
                }
                LinkRow(title: T("RootLogCard.Header"), detail: vm.rootElevationLog) {
                    Shell.open(folder: Shell.serviceLogFolder(vm.logDir))
                }
                Text(T("RootNote.Text")).font(.callout).foregroundStyle(.secondary)
            }
            Section(T("DiagnosticsHeader.Text")) {
                AdministratorSwitchRow(
                    title: T("InputTraceCard.Header"), detail: T("InputTraceCard.Description"), isOn: vm.inputTrace,
                    canChange: vm.canChangeTraces, needsAdministrator: vm.tracesNeedAdministrator, set: { await vm.setInputTrace($0) }) {
                    Task { await changeAsAdministrator(vm, ["settings", "set", "input-trace", vm.inputTrace ? "off" : "on"]) }
                }
                AdministratorSwitchRow(
                    title: T("ResizeTraceCard.Header"), detail: T("ResizeTraceCard.Description"), isOn: vm.resizeTrace,
                    canChange: vm.canChangeTraces, needsAdministrator: vm.tracesNeedAdministrator, set: { await vm.setResizeTrace($0) }) {
                    Task { await changeAsAdministrator(vm, ["settings", "set", "resize-trace", vm.resizeTrace ? "off" : "on"]) }
                }
                Text(T("DiagnosticsNote.Text")).font(.callout).foregroundStyle(.secondary)
            }
            Section(T("LogsHeader.Text")) {
                LinkRow(title: T("UserLogsCard.Header"), detail: T("UserLogsCard.Description")) {
                    Shell.open(folder: Shell.userLogFolder)
                }
                LinkRow(title: T("ServiceLogsCard.Header"), detail: vm.logDir) {
                    Shell.open(folder: Shell.serviceLogFolder(vm.logDir))
                }
            }
            Section(T("CommandLineHeader.Text")) {
                RowLabel(title: T("CommandLineCard.Header"), detail: T("CommandLineCard.Description"))
                ForEach(SettingsViewModel.commandLineCommands, id: \.self) { command in
                    CommandRow(command: command)
                }
            }
        }
        .settingsPage()
        .task { await vm.load() }
    }
}

/// One log line as columns: time, PID, TID, priority and the body. The app's font is
/// proportional, so the fields have fixed widths to line up; the body takes the rest.
struct LogRow: View {
    let line: LogLine

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 0) {
            Text(line.timeColumn).frame(width: 164, alignment: .leading)
            Text(line.pidColumn).frame(width: 56, alignment: .trailing)
            Text(line.tidColumn).frame(width: 56, alignment: .trailing)
            Text(line.priorityColumn).frame(width: 32, alignment: .center)
            Text(line.bodyColumn).padding(.leading, 6)
        }
    }
}

/// Android log: Android's log as it comes, filtered by priority and text, which can be paused,
/// and saving the diagnostics.
struct LogsPage: View {
    @ObservedObject var vm: LogViewModel
    @State private var selection = Set<LogLine.ID>()

    /// Puts the lines on the general pasteboard, each as the log prints it.
    private func copy(_ ids: Set<LogLine.ID>) {
        let text = vm.copyText(ids)
        if text.isEmpty {
            return
        }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(text, forType: .string)
    }

    private func copyProviders(_ ids: Set<LogLine.ID>) -> [NSItemProvider] {
        let text = vm.copyText(ids)
        return text.isEmpty ? [] : [NSItemProvider(object: text as NSString)]
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(T("LogsNote.Text")).font(.callout).foregroundStyle(.secondary)
            if let error = vm.error {
                Banner(kind: .error, title: T("LogsErrorBar.Title"), message: error)
            }
            if let status = vm.status {
                Banner(kind: vm.isSaving ? .info : .success, title: T("LogsStatusBar.Title"), message: status)
            }
            HStack {
                TextField(T("LogsFilter.PlaceholderText"), text: $vm.filter).textFieldStyle(.roundedBorder)
                Picker("", selection: $vm.priority) {
                    ForEach(LogViewModel.priorities, id: \.self) { priority in
                        Text(T("Logs_Priority_\(priority)")).tag(priority)
                    }
                }
                .labelsHidden()
                .frame(width: 140)
                Toggle(T("LogsPause.Content"), isOn: $vm.paused).toggleStyle(.button)
            }
            ScrollViewReader { reader in
                // Lines select as rows of any Mac list (click, Shift and Command clicks,
                // Command-A) and copy with Command-C, Edit > Copy or the context menu, as the
                // whole lines of the log.
                List(selection: $selection) {
                    ForEach(vm.lines) { line in
                        LogRow(line: line)
                            .foregroundStyle(color(line.priority))
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .listRowInsets(EdgeInsets(top: 0, leading: 8, bottom: 0, trailing: 8))
                            .id(line.id)
                    }
                }
                .listStyle(.plain)
                .scrollContentBackground(.hidden)
                .onCopyCommand { copyProviders(selection) }
                .contextMenu(forSelectionType: LogLine.ID.self) { ids in
                    Button(T("LogsCopy.Content")) { copy(ids) }
                        .disabled(ids.isEmpty)
                }
                .background(Color(nsColor: .textBackgroundColor))
                .clipShape(RoundedRectangle(cornerRadius: 6))
                .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(.quaternary))
                .onChange(of: vm.lines.count) { _ in
                    // The newest line stays in view while the log is not paused.
                    if !vm.paused, let last = vm.lines.last {
                        reader.scrollTo(last.id, anchor: .bottom)
                    }
                }
            }
            if vm.paused {
                Text(vm.pausedText).font(.callout).foregroundStyle(.secondary)
            }
        }
        .padding(20)
        .toolbar {
            Button {
                if let path = Shell.askDiagnosticsPath() {
                    Task { await vm.saveDiagnostics(path) }
                }
            } label: {
                Label(T("LogsSaveButton.Content"), systemImage: "square.and.arrow.down")
            }
            .disabled(!vm.canSave)
        }
        // Android reads its log for this page only while the page is shown.
        .onAppear { vm.start() }
        .onDisappear { vm.stop() }
    }

    private func color(_ priority: String) -> Color {
        switch priority {
        case "E", "F": return .red
        case "W": return .orange
        case "V", "D": return .secondary
        default: return .primary
        }
    }
}
