import XCTest
@testable import DsaSettingsCore

/// The Backup item on the Advanced page, through `dsa backup` as the other settings apps run
/// it.
@MainActor
final class BackupTests: XCTestCase {
    private let ready = #"""
    {"enabled":true,"available":true,"problem":"","folder":"/Users/me/DSA Backup","last_backup_unix_ms":1762729233000,
    "last_restore_unix_ms":0,"packages":12,"size":52428800,"last_error":"","succeeded":0,"failed":0,"installed":0}
    """#

    private let never = #"""
    {"enabled":true,"available":true,"problem":"","folder":"/Users/me/DSA Backup","last_backup_unix_ms":0,
    "last_restore_unix_ms":0,"packages":0,"size":0,"last_error":"","succeeded":0,"failed":0,"installed":0}
    """#

    private let unavailable = #"""
    {"enabled":true,"available":false,"problem":"the backup folder was made with another key; give its recovery key",
    "folder":"/Users/me/DSA Backup","last_backup_unix_ms":0,"last_restore_unix_ms":0,"packages":3,"size":1024,
    "last_error":"","succeeded":0,"failed":0,"installed":0}
    """#

    private func make() -> (FakeCli, BackupViewModel) {
        let cli = FakeCli()
        return (cli, BackupViewModel(service: DsaService(cli: cli), text: KeyText()))
    }

    func test_fr36_backup_status_reads_the_service_report() async {
        let info = DsaService.readBackup(try! Json.parse(ready))
        XCTAssertEqual(info, BackupInfo(
            enabled: true, available: true, problem: "", folder: "/Users/me/DSA Backup", lastBackupMs: 1_762_729_233_000,
            lastRestoreMs: 0, packages: 12, size: 52_428_800, lastError: "", succeeded: 0, failed: 0, installed: 0))

        let (cli, vm) = make()
        XCTAssertFalse(vm.canBackUp, "nothing is offered before the service has answered")
        XCTAssertFalse(vm.canImportRecoveryKey)
        XCTAssertEqual(vm.lastBackupText, "Backup_Unknown")
        cli.answer("backup status --json", ready)
        await vm.load()
        XCTAssertTrue(cli.called("backup status --json"))
        XCTAssertEqual(vm.folder, "/Users/me/DSA Backup")
        XCTAssertNotEqual(vm.lastBackupText, "Backup_Never")
        XCTAssertEqual(vm.lastBackupText, BackupViewModel.dateTime(1_762_729_233_000))
        XCTAssertEqual(vm.contentsText, "Backup_Contents")
        XCTAssertEqual(vm.lastRestoreText, "")
        XCTAssertNil(vm.problemText)
        XCTAssertNil(vm.lastErrorText)
        XCTAssertTrue(vm.canBackUp)
        XCTAssertTrue(vm.canRestore)
        XCTAssertTrue(vm.canShowRecoveryKey)
        XCTAssertTrue(vm.canImportRecoveryKey)
    }

    func test_fr36_a_backup_read_from_an_older_service_reads_as_empty() {
        let info = DsaService.readBackup(try! Json.parse(#"{"available":true}"#))
        XCTAssertTrue(info.available)
        XCTAssertEqual(info.folder, "")
        XCTAssertEqual(info.lastBackupMs, 0)
        XCTAssertEqual(info.packages, 0)
        XCTAssertEqual(info.size, 0)
        XCTAssertEqual(info.lastError, "")
    }

    func test_fr36_a_backup_never_made_says_never() async {
        let (cli, vm) = make()
        cli.answer("backup status --json", never)
        await vm.load()
        XCTAssertEqual(vm.lastBackupText, "Backup_Never")
    }

    func test_fr36_the_backup_time_is_in_the_users_date_format() {
        let text = BackupViewModel.dateTime(
            1_762_729_233_000, locale: Locale(identifier: "en_US"), timeZone: TimeZone(identifier: "UTC")!)
        XCTAssertTrue(text.contains("2025"), text)
        XCTAssertTrue(text.contains("Nov"), text)
    }

    func test_fr36_an_unavailable_backup_shows_its_problem() async {
        let (cli, vm) = make()
        cli.answer("backup status --json", unavailable)
        cli.answer("backup now --json", ready)
        await vm.load()
        XCTAssertEqual(vm.problemText, "the backup folder was made with another key; give its recovery key")
        XCTAssertFalse(vm.canBackUp)
        XCTAssertFalse(vm.canRestore)
        XCTAssertTrue(vm.canImportRecoveryKey, "a recovery key is the way to open a backup made with another key")
        await vm.backUpNow()
        XCTAssertFalse(cli.called("backup now --json"), "the backup cannot be used")

        cli.answer("backup status --json", #"{"enabled":true,"available":false,"problem":""}"#)
        await vm.load()
        XCTAssertEqual(vm.problemText, "Backup_UnavailableUnknown")
    }

    func test_fr36_the_last_error_is_shown() async {
        let (cli, vm) = make()
        cli.answer("backup status --json", ready.replacingOccurrences(
            of: #""last_error":"""#, with: #""last_error":"com.example.notes did not answer""#))
        await vm.load()
        XCTAssertEqual(vm.lastErrorText, "com.example.notes did not answer")
    }

    func test_fr36_back_up_now_shows_how_many_apps_were_backed_up() async {
        let (cli, vm) = make()
        cli.answer("backup status --json", never)
        cli.answer("backup now --json", ready.replacingOccurrences(of: #""succeeded":0"#, with: #""succeeded":12"#))
        await vm.load()
        await vm.backUpNow()
        XCTAssertTrue(cli.called("backup now --json"))
        XCTAssertEqual(vm.info?.succeeded, 12)
        XCTAssertEqual(vm.result, "Backup_Done")
        XCTAssertEqual(vm.lastBackupText, BackupViewModel.dateTime(1_762_729_233_000))
        XCTAssertFalse(vm.isBusy)
        XCTAssertNil(vm.progressText)
        XCTAssertNil(vm.error)

        cli.answer("backup now --json", ready
            .replacingOccurrences(of: #""succeeded":0"#, with: #""succeeded":10"#)
            .replacingOccurrences(of: #""failed":0"#, with: #""failed":2"#))
        await vm.backUpNow()
        XCTAssertEqual(vm.info?.failed, 2)
        XCTAssertEqual(vm.result, "Backup_DoneWithFailures")
    }

    func test_fr36_restore_shows_what_was_restored_and_installed() async {
        let (cli, vm) = make()
        cli.answer("backup status --json", ready)
        cli.answer("backup restore --json", ready
            .replacingOccurrences(of: #""last_restore_unix_ms":0"#, with: #""last_restore_unix_ms":1762729300000"#)
            .replacingOccurrences(of: #""succeeded":0"#, with: #""succeeded":12"#)
            .replacingOccurrences(of: #""installed":0"#, with: #""installed":4"#))
        await vm.load()
        await vm.restore()
        XCTAssertTrue(cli.called("backup restore --json"))
        XCTAssertEqual(vm.info?.installed, 4)
        XCTAssertEqual(vm.result, "Backup_Restored")
        XCTAssertEqual(vm.lastRestoreText, BackupViewModel.dateTime(1_762_729_300_000))

        cli.answer("backup restore --json", ready.replacingOccurrences(of: #""failed":0"#, with: #""failed":1"#))
        await vm.restore()
        XCTAssertEqual(vm.result, "Backup_RestoredWithFailures")
        XCTAssertEqual(vm.restoreWarning, "Backup_RestoreWarning")
    }

    func test_fr36_a_failed_backup_shows_why_and_keeps_the_state() async {
        let (cli, vm) = make()
        cli.answer("backup status --json", ready)
        cli.fail("backup now --json", "dsa: Android did not start")
        await vm.load()
        await vm.backUpNow()
        XCTAssertEqual(vm.error, "Problem_Refused (Android did not start)")
        XCTAssertNil(vm.result)
        XCTAssertEqual(vm.info?.packages, 12)
        XCTAssertTrue(vm.canBackUp)
    }

    func test_fr36_the_recovery_key_is_shown_until_closed() async {
        let (cli, vm) = make()
        cli.answer("backup status --json", ready)
        cli.answer("backup key show --json", #"{"recovery_key":"apple river stone cloud"}"#)
        await vm.load()
        await vm.showRecoveryKey()
        XCTAssertTrue(cli.called("backup key show --json"))
        XCTAssertEqual(vm.recoveryKey, "apple river stone cloud")
        XCTAssertFalse(vm.isBusy)
        vm.closeRecoveryKey()
        XCTAssertNil(vm.recoveryKey)

        cli.answer("backup key show --json", #"{"recovery_key":""}"#)
        await vm.showRecoveryKey()
        XCTAssertNil(vm.recoveryKey)
        XCTAssertEqual(vm.error, "Backup_KeyMissing")
    }

    func test_fr36_recovery_key_import_passes_the_words() async {
        let (cli, vm) = make()
        cli.answer("backup status --json", unavailable)
        cli.answer("backup key import apple river stone cloud --json", ready)
        await vm.load()
        XCTAssertFalse(vm.canImport("   \n "), "an empty key is not sent")
        let accepted = await vm.importRecoveryKey("  apple river\nstone   cloud ")
        XCTAssertTrue(accepted)
        XCTAssertTrue(cli.called("backup key import apple river stone cloud --json"))
        XCTAssertNil(vm.keyError)
        XCTAssertEqual(vm.result, "Backup_KeyImported")
        XCTAssertNil(vm.problemText, "the backup reads with the key now")
        XCTAssertTrue(vm.canBackUp)
    }

    func test_fr36_a_wrong_recovery_key_shows_the_refusal() async {
        let (cli, vm) = make()
        cli.answer("backup status --json", unavailable)
        cli.fail("backup key import wrong words --json", "dsa: the recovery key does not open this backup folder")
        await vm.load()
        let accepted = await vm.importRecoveryKey("wrong words")
        XCTAssertFalse(accepted)
        XCTAssertEqual(vm.keyError, "Problem_Refused (the recovery key does not open this backup folder)")
        XCTAssertNil(vm.result)
        XCTAssertEqual(vm.problemText, "the backup folder was made with another key; give its recovery key")
        vm.clearKeyError()
        XCTAssertNil(vm.keyError)
    }

    private let off = #"""
    {"enabled":false,"available":false,"problem":"","folder":"/Users/me/DSA Backup","last_backup_unix_ms":0,
    "last_restore_unix_ms":0,"packages":0,"size":0,"last_error":"","succeeded":0,"failed":0,"installed":0,
    "keymint_key":{"kept":true,"sealed_by":"secure-enclave","hardware":true,"description":"the Mac's Secure Enclave",
    "fallback_reason":"","problem":""}}
    """#

    private let offKept = #"""
    {"enabled":false,"available":false,"problem":"","folder":"/Users/me/DSA Backup","last_backup_unix_ms":1762729233000,
    "last_restore_unix_ms":0,"packages":12,"size":52428800,"last_error":"","succeeded":0,"failed":0,"installed":0}
    """#

    func test_fr36_backup_is_off_until_turned_on_and_off_is_no_problem() async {
        let (cli, vm) = make()
        cli.answer("backup status --json", off)
        cli.answer("backup on --json", never)
        await vm.load()
        XCTAssertFalse(vm.enabled)
        XCTAssertNil(vm.problemText, "off is the user's choice, not a problem")
        XCTAssertTrue(vm.canSwitch)
        XCTAssertFalse(vm.canBackUp)
        XCTAssertFalse(vm.canShowRecoveryKey)
        XCTAssertFalse(vm.canImportRecoveryKey)
        XCTAssertEqual(vm.switchDetail, "Backup_SwitchDetail")
        await vm.setEnabled(true)
        XCTAssertTrue(cli.called("backup on --json"))
        XCTAssertTrue(vm.enabled)
        XCTAssertEqual(vm.result, "Backup_TurnedOn")
        XCTAssertTrue(vm.canBackUp)
    }

    func test_fr36_turning_backup_off_keeps_or_deletes_the_backups() async {
        let (cli, vm) = make()
        cli.answer("backup status --json", ready)
        cli.answer("backup off --json", offKept)
        cli.answer("backup off --delete-backups --json", off)
        await vm.load()
        XCTAssertTrue(vm.hasBackupsKept)
        XCTAssertEqual(vm.deleteQuestion, "Backup_DeleteQuestion")
        await vm.setEnabled(false)
        XCTAssertTrue(cli.called("backup off --json"))
        XCTAssertEqual(vm.result, "Backup_TurnedOff")
        XCTAssertEqual(vm.switchDetail, "Backup_SwitchDetail\nBackup_OffKept")
        await vm.setEnabled(false, deleteBackups: true)
        XCTAssertTrue(cli.called("backup off --delete-backups --json"))
        XCTAssertEqual(vm.result, "Backup_TurnedOffDeleted")
        XCTAssertFalse(vm.hasBackupsKept)
    }

    func test_fr36_the_item_says_what_seals_androids_keys() async {
        let (cli, vm) = make()
        cli.answer("backup status --json", off)
        await vm.load()
        XCTAssertEqual(vm.keyProtectionText, "KeyProtection_SecureEnclave")
        XCTAssertNil(vm.keyProtectionReason)
        XCTAssertNil(vm.keyProblem)

        let keychain = DsaService.readKeyProtection(try! Json.parse(#"""
        {"kept":true,"sealed_by":"keychain","hardware":false,"description":"your login keychain",
        "fallback_reason":"this Mac has no Secure Enclave DSA can use","problem":"the Mac has not been unlocked"}
        """#))
        XCTAssertEqual(keychain?.sealedBy, "keychain")
        XCTAssertEqual(keychain?.fallbackReason, "this Mac has no Secure Enclave DSA can use")
        XCTAssertNil(DsaService.readKeyProtection(try! Json.parse(#"{"keymint_key":null}"#)["keymint_key"]))
        XCTAssertNil(DsaService.readBackup(try! Json.parse(ready)).keymintKey, "an older service says nothing")
    }
}