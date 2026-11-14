import DsaSettingsCore
import SwiftUI

/// The Backup item: whether backup is on (off until the user turns it on, since it keeps a
/// second copy of the apps' user data on this Mac), where the encrypted backup is kept and what
/// it holds, backing up and restoring now, the recovery key that opens it on another computer,
/// and how Android's own keys are protected on this Mac.
struct BackupSection: View {
    @ObservedObject var vm: BackupViewModel
    @State private var importing = false
    @State private var keyDraft = ""

    var body: some View {
        Section(T("BackupHeader.Text")) {
            SwitchRow(title: T("BackupSwitch.Header"), detail: vm.switchDetail, isOn: vm.enabled,
                      enabled: vm.canSwitch) { on in
                await switchBackup(on)
            }
            if let error = vm.error {
                Banner(kind: .error, title: T("BackupErrorBar.Title"), message: error)
            }
            if let problem = vm.problemText {
                Banner(kind: .warning, title: T("BackupUnavailableBar.Title"), message: problem)
            }
            if let progress = vm.progressText {
                HStack(spacing: 8) {
                    ProgressView().controlSize(.small)
                    Note(text: progress)
                }
            }
            if let result = vm.result {
                Banner(kind: .success, title: T("BackupDoneBar.Title"), message: result)
            }
            if vm.enabled {
                backupRows
            }
            if vm.keyProtection != nil {
                if let problem = vm.keyProblem {
                    Banner(kind: .error, title: T("KeyProblemBar.Title"), message: problem)
                }
                ValueRow(title: T("KeyProtectionCard.Header"), value: vm.keyProtectionText)
                if let reason = vm.keyProtectionReason {
                    Note(text: reason)
                }
            }
        }
        .sheet(isPresented: Binding(get: { vm.recoveryKey != nil }, set: { shown in if !shown { vm.closeRecoveryKey() } })) {
            RecoveryKeySheet(key: vm.recoveryKey ?? "") { vm.closeRecoveryKey() }
        }
        .sheet(isPresented: $importing) {
            ImportKeySheet(vm: vm, key: $keyDraft) { importing = false }
        }
        .task { await vm.load() }
    }

    /// Turns backup on, or off: off while backups are kept asks whether they go too, and
    /// cancelling leaves backup on.
    @MainActor private func switchBackup(_ on: Bool) async {
        var delete = false
        if !on && vm.hasBackupsKept {
            guard let answer = Shell.ask(T("BackupOffConfirm.Title"), vm.deleteQuestion,
                                         yes: T("BackupOffConfirm.Delete"), no: T("BackupOffConfirm.Keep")) else {
                return
            }
            delete = answer
        }
        await vm.setEnabled(on, deleteBackups: delete)
    }

    /// The rows that mean something only while backup is on.
    @ViewBuilder private var backupRows: some View {
        LinkRow(title: T("BackupFolderCard.Header"), detail: vm.folder, enabled: !vm.folder.isEmpty) {
            Shell.open(folder: URL(fileURLWithPath: vm.folder))
        }
        ValueRow(title: T("BackupLastCard.Header"), value: vm.lastBackupText)
        if !vm.lastRestoreText.isEmpty {
            ValueRow(title: T("BackupLastRestoreCard.Header"), value: vm.lastRestoreText)
        }
        ValueRow(title: T("BackupContentsCard.Header"), value: vm.contentsText)
        if let lastError = vm.lastErrorText {
            ValueRow(title: T("BackupLastErrorCard.Header"), value: lastError)
        }
        ButtonRow(title: T("BackupNowCard.Header"), detail: T("BackupNowCard.Description"),
                  action: T("BackupNowButton.Content"), enabled: vm.canBackUp, busy: vm.isBackingUp) {
            Task { await vm.backUpNow() }
        }
        ButtonRow(title: T("BackupRestoreCard.Header"), detail: T("BackupRestoreCard.Description"),
                  action: T("BackupRestoreButton.Content"), enabled: vm.canRestore, busy: vm.isRestoring) {
            guard Shell.confirm(T("BackupRestoreConfirm.Title"), vm.restoreWarning, T("BackupRestoreConfirm.Action")) else {
                return
            }
            Task { await vm.restore() }
        }
        ButtonRow(title: T("BackupKeyCard.Header"), detail: T("BackupKeyCard.Description"),
                  action: T("BackupKeyShowButton.Content"), enabled: vm.canShowRecoveryKey,
                  busy: vm.running == .readingKey) {
            Task { await vm.showRecoveryKey() }
        }
        ButtonRow(title: T("BackupKeyImportCard.Header"), detail: T("BackupKeyImportCard.Description"),
                  action: T("BackupKeyImportButton.Content"), enabled: vm.canImportRecoveryKey) {
            keyDraft = ""
            vm.clearKeyError()
            importing = true
        }
    }
}

/// The recovery key, selectable and with a copy button, and why to write it down.
struct RecoveryKeySheet: View {
    let key: String
    let close: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text(T("BackupKeySheet.Title")).font(.headline)
            Text(T("BackupKeySheet.Message")).foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            HStack(alignment: .top) {
                Text(key)
                    .font(.system(.title3, design: .monospaced))
                    .textSelection(.enabled)
                    .fixedSize(horizontal: false, vertical: true)
                    .frame(maxWidth: .infinity, alignment: .leading)
                CopyButton(text: key)
            }
            .padding(12)
            .background(.quaternary.opacity(0.5), in: RoundedRectangle(cornerRadius: 8))
            HStack {
                Spacer()
                Button(T("BackupKeySheetDone.Content"), action: close)
                    .keyboardShortcut(.defaultAction)
            }
        }
        .padding(20)
        .frame(width: 460)
    }
}

/// Asks for a recovery key and sends it, keeping the sheet open with the reason when the
/// service refuses it.
struct ImportKeySheet: View {
    @ObservedObject var vm: BackupViewModel
    @Binding var key: String
    let close: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text(T("BackupImportSheet.Title")).font(.headline)
            Text(T("BackupImportSheet.Message")).foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            TextField(T("BackupImportSheet.PlaceholderText"), text: $key)
                .textFieldStyle(.roundedBorder)
                .font(.system(.body, design: .monospaced))
                .disabled(vm.isImportingKey)
                .onSubmit { send() }
            if let refused = vm.keyError {
                Banner(kind: .error, title: T("BackupImportRefused.Title"), message: refused)
            }
            HStack {
                if vm.isImportingKey {
                    ProgressView().controlSize(.small)
                }
                Spacer()
                Button(T("CancelButton"), action: close)
                    .keyboardShortcut(.cancelAction)
                    .disabled(vm.isImportingKey)
                Button(T("BackupImportSheetUse.Content")) { send() }
                    .keyboardShortcut(.defaultAction)
                    .disabled(!vm.canImport(key))
            }
        }
        .padding(20)
        .frame(width: 460)
    }

    @MainActor private func send() {
        let typed = key
        guard vm.canImport(typed) else {
            return
        }
        Task {
            if await vm.importRecoveryKey(typed) {
                close()
            }
        }
    }
}
