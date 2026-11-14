import Foundation

/// How the root key of Android's KeyMint is sealed on this Mac, as `dsa backup` reports it under
/// `keymint_key`.
public struct KeyProtectionInfo: Equatable, Sendable {
    /// Whether a key is kept yet; one is made when Android first starts.
    public var kept: Bool
    /// `secure-enclave`, `keychain` (and on other hosts `tpm`, `dpapi`, `secret-service`).
    public var sealedBy: String
    /// Whether hardware of this Mac holds what seals it.
    public var hardware: Bool
    /// What seals it, in the service's words.
    public var description: String
    /// Why the Secure Enclave does not seal it, when it does not.
    public var fallbackReason: String
    /// Why the latest start of Android could not open it, and what to do.
    public var problem: String
}

/// Where the user's app data backup stands, as `dsa backup` reports it.
public struct BackupInfo: Equatable, Sendable {
    /// Whether the user turned backup on. Off until they do: it keeps a second copy of the
    /// apps' user data on this Mac.
    public var enabled: Bool = false
    /// Whether the service can back up and restore now.
    public var available: Bool
    /// Why it cannot, in the service's words; empty while it can.
    public var problem: String
    /// The folder the encrypted backup is kept in.
    public var folder: String
    /// When the last backup finished, in milliseconds since the Unix epoch; 0 is never.
    public var lastBackupMs: Int64
    /// When the last restore finished, in milliseconds since the Unix epoch; 0 is never.
    public var lastRestoreMs: Int64
    /// How many apps the backup holds.
    public var packages: Int
    /// The size of their data before encryption, in bytes.
    public var size: UInt64
    /// What went wrong last; empty when nothing did.
    public var lastError: String
    /// After backing up or restoring: how many apps it worked for and how many it did not.
    public var succeeded: Int
    public var failed: Int
    /// After restoring: how many apps of the backup were installed first.
    public var installed: Int
    /// How Android's KeyMint root key is sealed; nil from a service that does not say.
    public var keymintKey: KeyProtectionInfo? = nil
}

public extension DsaService {
    /// Reads what `dsa backup` printed. A field a service does not report reads as empty.
    static func readBackup(_ json: Json) -> BackupInfo {
        BackupInfo(
            enabled: json.bool("enabled"),
            available: json.bool("available"),
            problem: json.string("problem"),
            folder: json.string("folder"),
            lastBackupMs: json.int64("last_backup_unix_ms"),
            lastRestoreMs: json.int64("last_restore_unix_ms"),
            packages: Int(clamping: json.uint("packages")),
            size: json.uint("size"),
            lastError: json.string("last_error"),
            succeeded: Int(clamping: json.uint("succeeded")),
            failed: Int(clamping: json.uint("failed")),
            installed: Int(clamping: json.uint("installed")),
            keymintKey: readKeyProtection(json["keymint_key"]))
    }

    /// Reads `keymint_key`; nil when it is not an object.
    static func readKeyProtection(_ key: Json) -> KeyProtectionInfo? {
        guard key.isObject else {
            return nil
        }
        return KeyProtectionInfo(
            kept: key.bool("kept"),
            sealedBy: key.string("sealed_by"),
            hardware: key.bool("hardware"),
            description: key.string("description"),
            fallbackReason: key.string("fallback_reason"),
            problem: key.string("problem"))
    }

    /// The words of a recovery key as the user typed or pasted it: dsa takes them as separate
    /// arguments, however many spaces or line breaks were between them.
    static func recoveryKeyWords(_ key: String) -> [String] {
        key.split(whereSeparator: { $0.isWhitespace }).map(String.init)
    }

    func backup() async throws -> BackupInfo {
        DsaService.readBackup(try await runJson(["backup", "status"]))
    }

    /// Turns backup on, or off; off with `deleteBackups`, the backups kept so far and their key
    /// are removed as well.
    func setBackup(_ on: Bool, deleteBackups: Bool) async throws -> BackupInfo {
        let args = on ? ["backup", "on"] : deleteBackups ? ["backup", "off", "--delete-backups"] : ["backup", "off"]
        return DsaService.readBackup(try await runJson(args))
    }

    /// Has Android back up every app now, starting Android first when it is not running. It
    /// waits for Android, which can take long.
    func backupNow() async throws -> BackupInfo {
        DsaService.readBackup(try await runJson(["backup", "now"]))
    }

    /// Installs the apps of the backup that Android does not have and puts every app's data
    /// back from the backup, starting Android first when it is not running.
    func restoreBackup() async throws -> BackupInfo {
        DsaService.readBackup(try await runJson(["backup", "restore"]))
    }

    /// The backup's recovery key, for the user to write down.
    func backupRecoveryKey() async throws -> String {
        try await runJson(["backup", "key", "show"]).string("recovery_key")
    }

    /// Keeps `key` as the backup's key, so a backup folder made on another computer, or for
    /// another user, can be read. The service refuses a key that does not open the backup.
    func importBackupRecoveryKey(_ key: String) async throws -> BackupInfo {
        DsaService.readBackup(try await runJson(["backup", "key", "import"] + DsaService.recoveryKeyWords(key)))
    }
}

/// The Backup item on the Advanced page: where the user's app data backup stands, backing up
/// and restoring now, and the recovery key that opens the backup on another computer.
@MainActor
public final class BackupViewModel: ObservableObject {
    /// What runs now.
    public enum Action: Equatable, Sendable {
        case backingUp
        case restoring
        case readingKey
        case importingKey
        case switching
    }

    private let service: DsaService
    private let text: TextSource

    @Published public private(set) var info: BackupInfo?
    @Published public private(set) var error: String?
    @Published public private(set) var running: Action?
    /// How the last backup, restore or key import went, to show once it has finished.
    @Published public private(set) var result: String?
    /// The recovery key while it is shown; nil once the user closes it.
    @Published public private(set) var recoveryKey: String?
    /// Why the service refused the recovery key the user typed.
    @Published public private(set) var keyError: String?

    public init(service: DsaService, text: TextSource) {
        self.service = service
        self.text = text
    }

    public var loaded: Bool { info != nil }
    public var isBusy: Bool { running != nil }
    public var isBackingUp: Bool { running == .backingUp }
    public var isRestoring: Bool { running == .restoring }
    public var isImportingKey: Bool { running == .importingKey }
    public var available: Bool { info?.available ?? false }
    /// Whether the user turned backup on. Off until they do.
    public var enabled: Bool { info?.enabled ?? false }

    public var canSwitch: Bool { loaded && !isBusy }
    public var canBackUp: Bool { loaded && available && !isBusy }
    public var canRestore: Bool { loaded && available && !isBusy }
    public var canShowRecoveryKey: Bool { loaded && enabled && !isBusy }
    /// A key can be given while the backup cannot be read, since that is what it is for; only
    /// while backup is on.
    public var canImportRecoveryKey: Bool { loaded && enabled && !isBusy }

    /// Whether turning backup off leaves backups behind, which the page offers to delete.
    public var hasBackupsKept: Bool { (info?.packages ?? 0) > 0 }

    /// What the switch says under its title: what backup keeps, and while it is off, what
    /// earlier backups are still kept.
    public var switchDetail: String {
        guard let info, !info.enabled, info.packages > 0 else {
            return text.get("Backup_SwitchDetail")
        }
        return text.get("Backup_SwitchDetail") + "\n"
            + text.format("Backup_OffKept", info.packages, AppsViewModel.size(info.size))
    }

    /// What the page asks when backup is turned off while backups are kept.
    public var deleteQuestion: String {
        text.format("Backup_DeleteQuestion", info?.packages ?? 0, AppsViewModel.size(info?.size ?? 0))
    }

    /// How Android's keys are protected; nil from a service that does not say.
    public var keyProtection: KeyProtectionInfo? { info?.keymintKey }

    /// What protects Android's keys on this Mac, in the user's language.
    public var keyProtectionText: String {
        guard let key = keyProtection else { return "" }
        if !key.kept { return text.get("KeyProtection_None") }
        switch key.sealedBy {
        case "secure-enclave": return text.get("KeyProtection_SecureEnclave")
        case "keychain": return text.get("KeyProtection_Keychain")
        default:
            return key.hardware
                ? text.format("KeyProtection_Hardware", key.description)
                : text.format("KeyProtection_Store", key.description)
        }
    }

    /// Why the Secure Enclave does not seal Android's keys, when it does not.
    public var keyProtectionReason: String? {
        guard let key = keyProtection, key.kept, !key.hardware, !key.fallbackReason.isEmpty else {
            return nil
        }
        return text.format("KeyProtection_Why", key.fallbackReason)
    }

    /// Why the latest start of Android could not open its keys.
    public var keyProblem: String? {
        guard let problem = keyProtection?.problem, !problem.isEmpty else { return nil }
        return problem
    }

    /// Whether `key` has anything to send.
    public func canImport(_ key: String) -> Bool {
        canImportRecoveryKey && !DsaService.recoveryKeyWords(key).isEmpty
    }

    public var folder: String { info?.folder ?? "" }

    /// When the last backup finished, in the user's date and time format, or never.
    public var lastBackupText: String {
        guard let info else {
            return text.get("Backup_Unknown")
        }
        return info.lastBackupMs > 0 ? BackupViewModel.dateTime(info.lastBackupMs) : text.get("Backup_Never")
    }

    /// When the last restore finished; empty when nothing was restored.
    public var lastRestoreText: String {
        guard let info, info.lastRestoreMs > 0 else {
            return ""
        }
        return BackupViewModel.dateTime(info.lastRestoreMs)
    }

    /// How many apps the backup holds and how much data.
    public var contentsText: String {
        guard let info else {
            return text.get("Backup_Unknown")
        }
        return text.format("Backup_Contents", info.packages, AppsViewModel.size(info.size))
    }

    /// Why the backup cannot be used now; nil while it can or before the service answered.
    /// Off is the user's choice, not a problem.
    public var problemText: String? {
        guard let info, !info.available else {
            return nil
        }
        if !info.enabled && info.problem.isEmpty {
            return nil
        }
        return info.problem.isEmpty ? text.get("Backup_UnavailableUnknown") : info.problem
    }

    /// What went wrong last; nil when nothing did.
    public var lastErrorText: String? {
        guard let info, !info.lastError.isEmpty else {
            return nil
        }
        return info.lastError
    }

    /// What runs now, in a line; nil while nothing runs.
    public var progressText: String? {
        switch running {
        case .backingUp: return text.get("Backup_BackingUp")
        case .restoring: return text.get("Backup_Restoring")
        case .readingKey, .importingKey, .switching, .none: return nil
        }
    }

    public var restoreWarning: String { text.get("Backup_RestoreWarning") }

    /// A time in milliseconds since the Unix epoch, in the user's date and time format.
    public nonisolated static func dateTime(_ ms: Int64, locale: Locale = .current, timeZone: TimeZone = .current) -> String {
        let formatter = DateFormatter()
        formatter.locale = locale
        formatter.timeZone = timeZone
        formatter.dateStyle = .medium
        formatter.timeStyle = .short
        return formatter.string(from: Date(timeIntervalSince1970: Double(ms) / 1000))
    }

    private func backedUpText(_ info: BackupInfo) -> String {
        info.failed > 0
            ? text.format("Backup_DoneWithFailures", info.succeeded, info.failed)
            : text.format("Backup_Done", info.succeeded)
    }

    private func restoredText(_ info: BackupInfo) -> String {
        info.failed > 0
            ? text.format("Backup_RestoredWithFailures", info.succeeded, info.installed, info.failed)
            : text.format("Backup_Restored", info.succeeded, info.installed)
    }

    /// Reads where the backup stands. A service that cannot say leaves the item unknown, with
    /// its buttons off.
    public func load() async {
        if isBusy { return }
        if let read = try? await service.backup() {
            info = read
        }
    }

    private func act(_ action: Action, _ work: () async throws -> BackupInfo, done: (BackupInfo) -> String) async {
        running = action
        error = nil
        result = nil
        do {
            let report = try await work()
            info = report
            result = done(report)
        } catch {
            self.error = Format.problem(text, error)
        }
        running = nil
    }

    /// Turns backup on, or off. The page asks first, when backups are kept, whether turning it
    /// off deletes them (`deleteBackups`); this does not ask again.
    public func setEnabled(_ on: Bool, deleteBackups: Bool = false) async {
        if !canSwitch { return }
        recoveryKey = nil
        await act(.switching, { try await service.setBackup(on, deleteBackups: !on && deleteBackups) }, done: { _ in
            on ? self.text.get("Backup_TurnedOn")
                : deleteBackups ? self.text.get("Backup_TurnedOffDeleted") : self.text.get("Backup_TurnedOff")
        })
    }

    /// Backs up every app now.
    public func backUpNow() async {
        if !canBackUp { return }
        await act(.backingUp, { try await service.backupNow() }, done: { self.backedUpText($0) })
    }

    /// Restores the apps and their data from the backup. The page asks the user first, with
    /// `restoreWarning`; this does not ask again.
    public func restore() async {
        if !canRestore { return }
        await act(.restoring, { try await service.restoreBackup() }, done: { self.restoredText($0) })
    }

    /// Reads the recovery key, which the page then shows until `closeRecoveryKey`.
    public func showRecoveryKey() async {
        if !canShowRecoveryKey { return }
        running = .readingKey
        error = nil
        do {
            let key = try await service.backupRecoveryKey()
            if key.isEmpty {
                error = text.get("Backup_KeyMissing")
            } else {
                recoveryKey = key
            }
        } catch {
            self.error = Format.problem(text, error)
        }
        running = nil
    }

    public func closeRecoveryKey() {
        recoveryKey = nil
    }

    /// Keeps `key` as the backup's key. True when the service took it; when it refused,
    /// `keyError` says why and the sheet stays open for another try.
    @discardableResult
    public func importRecoveryKey(_ key: String) async -> Bool {
        if !canImport(key) { return false }
        running = .importingKey
        keyError = nil
        result = nil
        var accepted = false
        do {
            info = try await service.importBackupRecoveryKey(key)
            result = text.get("Backup_KeyImported")
            accepted = true
        } catch {
            keyError = Format.problem(text, error)
        }
        running = nil
        return accepted
    }

    /// Forgets why a key was refused, when the user starts again.
    public func clearKeyError() {
        keyError = nil
    }
}
