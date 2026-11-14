using System.Globalization;
using Xunit;

namespace Dsa.Settings.Core.Tests;

/// <summary>The backup section on the Advanced page: what it shows, and that each button does
/// what <c>dsa backup</c> does.</summary>
public class BackupViewModelTests
{
    private const string Settings =
        "{\"render_gpu\":\"display\",\"freeze\":false,\"program_updates\":true,\"image_updates\":true," +
        "\"program_updates_available\":true,\"image_updates_available\":false,\"version\":\"0.0.9\"}";

    private const string Folder = "C:\\\\Users\\\\alice\\\\DSA Backup";

    private const string TpmKey =
        "{\"kept\":true,\"sealed_by\":\"tpm\",\"hardware\":true," +
        "\"description\":\"the computer's TPM 2.0 and Windows DPAPI under your account\"," +
        "\"fallback_reason\":\"\",\"problem\":\"\"}";

    private static string Report(bool available = true, string problem = "", long last = 1_700_000_000_000,
        int packages = 3, long size = 3 * 1024 * 1024, string lastError = "", int succeeded = 0, int failed = 0,
        int installed = 0, bool enabled = true, string keymint = TpmKey) =>
        "{\"enabled\":" + (enabled ? "true" : "false") + "," +
        "\"available\":" + (available ? "true" : "false") + ",\"problem\":\"" + problem + "\"," +
        "\"folder\":\"" + Folder + "\",\"last_backup_unix_ms\":" + last + ",\"last_restore_unix_ms\":0," +
        "\"packages\":" + packages + ",\"size\":" + size + ",\"last_error\":\"" + lastError + "\"," +
        "\"succeeded\":" + succeeded + ",\"failed\":" + failed + ",\"installed\":" + installed + "," +
        "\"keymint_key\":" + keymint + "}";

    public BackupViewModelTests()
    {
        CultureInfo.CurrentCulture = CultureInfo.InvariantCulture;
    }

    private static FakeCli Cli(string status) => new FakeCli()
        .Answer("settings --json", Settings)
        .Fail("gpus --json", "dsa: no GPU list")
        .Answer("backup status --json", status);

    private static async Task<SettingsViewModel> Loaded(FakeCli cli)
    {
        var vm = new SettingsViewModel(new DsaService(cli), new KeyText());
        await vm.LoadAsync();
        return vm;
    }

    [Fact]
    public async Task Fr36_the_section_shows_where_the_backup_stands()
    {
        var cli = Cli(Report(lastError: "com.example.notes: the app refused"));
        var vm = await Loaded(cli);
        Assert.Contains("backup status --json", cli.Calls);
        Assert.True(vm.HasBackup);
        Assert.True(vm.BackupAvailable);
        Assert.False(vm.HasBackupProblem);
        Assert.Equal(@"C:\Users\alice\DSA Backup", vm.Backup!.Folder);
        Assert.Contains(@"C:\Users\alice\DSA Backup", vm.BackupFolder);
        Assert.Equal(3UL, vm.Backup.Packages);
        Assert.Contains("3", vm.BackupApps);
        Assert.Contains("3.0 MB", vm.BackupApps);
        Assert.True(vm.HasBackupLastError);
        Assert.Contains("the app refused", vm.BackupLastError);
        Assert.True(vm.CanBackUpNow);
        Assert.True(vm.CanRestoreBackup);
        Assert.True(vm.CanShowRecoveryKey);
        Assert.True(vm.CanUseRecoveryKey);
        Assert.False(vm.HasRecoveryKey);
        Assert.True(vm.ShowsRecoveryKeyButton);
    }

    [Fact]
    public async Task Fr36_backup_is_off_until_turned_on_and_off_is_no_problem()
    {
        var cli = Cli(Report(enabled: false, available: false, packages: 0, last: 0))
            .Answer("backup on --json", Report(packages: 0, last: 0));
        var vm = await Loaded(cli);
        Assert.False(vm.BackupEnabled);
        Assert.True(vm.CanToggleBackup);
        Assert.False(vm.HasBackupProblem, "off is the user's choice, not a problem");
        Assert.Equal("Backup_Off", vm.BackupOffNote);
        Assert.False(vm.CanBackUpNow);
        Assert.False(vm.CanShowRecoveryKey);
        Assert.False(vm.CanUseRecoveryKey);
        Assert.False(vm.HasBackupsKept);

        await vm.SetBackupAsync(true);
        Assert.Contains("backup on --json", cli.Calls);
        Assert.True(vm.BackupEnabled);
        Assert.False(vm.HasBackupOffNote);
        Assert.Equal("Backup_TurnedOn", vm.BackupResult);
        Assert.True(vm.CanBackUpNow);
    }

    [Fact]
    public async Task Fr36_turning_backup_off_keeps_or_deletes_the_backups_as_the_user_says()
    {
        var cli = Cli(Report())
            .Answer("backup off --json", Report(enabled: false, available: false))
            .Answer("backup off --delete-backups --json", Report(enabled: false, available: false, packages: 0, size: 0));
        var vm = await Loaded(cli);
        Assert.True(vm.HasBackupsKept);
        Assert.Equal("Backup_DeleteQuestion", vm.BackupDeleteQuestion);

        await vm.SetBackupAsync(false);
        Assert.Contains("backup off --json", cli.Calls);
        Assert.False(vm.BackupEnabled);
        Assert.Equal("Backup_TurnedOff", vm.BackupResult);
        Assert.Equal("Backup_OffKept", vm.BackupOffNote);

        await vm.SetBackupAsync(false, deleteBackups: true);
        Assert.Contains("backup off --delete-backups --json", cli.Calls);
        Assert.Equal("Backup_TurnedOffDeleted", vm.BackupResult);
        Assert.Equal("Backup_Off", vm.BackupOffNote);
    }

    [Fact]
    public async Task Fr36_the_section_says_what_seals_androids_keys()
    {
        var vm = await Loaded(Cli(Report()));
        Assert.True(vm.HasKeyProtection);
        Assert.Equal("KeyProtection_Tpm", vm.KeyProtectionText);
        Assert.False(vm.KeyProtectionWeaker);
        Assert.False(vm.HasKeyProtectionDetail);
        Assert.False(vm.HasKeyProblem);

        var dpapi = "{\"kept\":true,\"sealed_by\":\"dpapi\",\"hardware\":false," +
            "\"description\":\"Windows DPAPI under your account\"," +
            "\"fallback_reason\":\"this computer has no TPM that DSA can use\"," +
            "\"problem\":\"the TPM is locked out\"}";
        vm = await Loaded(Cli(Report(keymint: dpapi)));
        Assert.Equal("KeyProtection_Dpapi", vm.KeyProtectionText);
        Assert.True(vm.KeyProtectionWeaker);
        Assert.Equal("KeyProtection_Why", vm.KeyProtectionDetail);
        Assert.True(vm.HasKeyProblem);
        Assert.Contains("locked out", vm.KeyProblem);

        var none = "{\"kept\":false,\"sealed_by\":\"\",\"hardware\":false,\"description\":\"\"," +
            "\"fallback_reason\":\"\",\"problem\":\"\"}";
        vm = await Loaded(Cli(Report(keymint: none)));
        Assert.Equal("KeyProtection_None", vm.KeyProtectionText);

        vm = await Loaded(Cli(Report(keymint: "null")));
        Assert.False(vm.HasKeyProtection, "a service that does not say shows nothing");
    }

    [Fact]
    public void Fr36_the_last_backup_time_is_shown_in_the_users_zone_and_never_when_there_is_none()
    {
        Assert.Equal("never", SettingsViewModel.BackupTime(0, TimeZoneInfo.Utc, CultureInfo.InvariantCulture, "never"));
        Assert.Equal("11/14/2023 22:13",
            SettingsViewModel.BackupTime(1_700_000_000_000, TimeZoneInfo.Utc, CultureInfo.InvariantCulture, "never"));
    }

    [Fact]
    public async Task Fr36_an_unavailable_backup_says_why_and_offers_only_a_recovery_key()
    {
        var cli = Cli(Report(available: false,
            problem: "the backup key kept for this user cannot be opened here", packages: 0, last: 0));
        var vm = await Loaded(cli);
        Assert.True(vm.HasBackupProblem);
        Assert.Contains("cannot be opened here", vm.BackupProblem);
        Assert.False(vm.CanBackUpNow);
        Assert.False(vm.CanRestoreBackup);
        Assert.False(vm.CanShowRecoveryKey);
        Assert.True(vm.CanUseRecoveryKey, "bringing the key is what opens the backup");
        Assert.Contains("Backup_Never", vm.BackupLast);
    }

    [Fact]
    public async Task Fr36_a_service_without_backups_leaves_the_section_saying_so()
    {
        var cli = new FakeCli()
            .Answer("settings --json", Settings)
            .Fail("gpus --json", "dsa: no GPU list")
            .Fail("backup status --json", "dsa: this user's Android keeps no backups");
        var vm = await Loaded(cli);
        Assert.False(vm.HasBackup);
        Assert.Equal("Backup_NoService", vm.BackupProblem);
        Assert.False(vm.CanBackUpNow);
        Assert.False(vm.CanUseRecoveryKey);
        Assert.False(vm.HasError, "the rest of the page is not an error");
    }

    [Fact]
    public async Task Fr36_back_up_now_runs_dsa_backup_now()
    {
        var cli = Cli(Report(packages: 0, last: 0))
            .Answer("backup now --json", Report(packages: 5, succeeded: 5));
        var vm = await Loaded(cli);
        Assert.False(vm.CanRestoreBackup, "nothing to restore yet");
        await vm.BackUpNowAsync();
        Assert.Contains("backup now --json", cli.Calls);
        Assert.Equal(5UL, vm.Backup!.Packages);
        Assert.True(vm.HasBackupResult);
        Assert.Equal("Backup_Done", vm.BackupResult);
        Assert.False(vm.HasBackupError);
        Assert.False(vm.IsBackupWorking);
        Assert.True(vm.CanRestoreBackup);
    }

    [Fact]
    public async Task Fr36_a_failed_backup_says_why_and_can_be_tried_again()
    {
        var cli = Cli(Report())
            .Fail("backup now --json", "dsa: Android did not start within 360 s");
        var vm = await Loaded(cli);
        await vm.BackUpNowAsync();
        Assert.True(vm.HasBackupError);
        Assert.Contains("did not start", vm.BackupError);
        Assert.False(vm.HasBackupResult);
        Assert.True(vm.CanBackUpNow);
    }

    [Fact]
    public async Task Fr36_restore_runs_dsa_backup_restore()
    {
        var cli = Cli(Report())
            .Answer("backup restore --json", Report(succeeded: 3, installed: 2, failed: 1));
        var vm = await Loaded(cli);
        Assert.Equal("Backup_RestoreWarning", vm.RestoreWarning);
        await vm.RestoreBackupAsync();
        Assert.Contains("backup restore --json", cli.Calls);
        Assert.Equal("Backup_RestoredSome", vm.BackupResult);
        Assert.Equal(2UL, vm.Backup!.Installed);
    }

    [Fact]
    public async Task Fr36_show_recovery_key_runs_dsa_backup_key_show_and_hides_again()
    {
        var cli = Cli(Report())
            .Answer("backup key show --json", "{\"recovery_key\":\"ABCD-EFGH-IJKL-MNOP\"}");
        var vm = await Loaded(cli);
        await vm.ShowRecoveryKeyAsync();
        Assert.Contains("backup key show --json", cli.Calls);
        Assert.True(vm.HasRecoveryKey);
        Assert.False(vm.ShowsRecoveryKeyButton);
        Assert.Equal("ABCD-EFGH-IJKL-MNOP", vm.RecoveryKey);
        Assert.Equal("Backup_KeyAdvice", vm.RecoveryKeyAdvice);
        vm.HideRecoveryKey();
        Assert.False(vm.HasRecoveryKey);
        Assert.Equal("", vm.RecoveryKey);
    }

    [Fact]
    public async Task Fr36_a_key_that_cannot_be_shown_says_why()
    {
        var cli = Cli(Report())
            .Fail("backup key show --json", "dsa: Windows opens this user's credentials only while they are signed in");
        var vm = await Loaded(cli);
        await vm.ShowRecoveryKeyAsync();
        Assert.False(vm.HasRecoveryKey);
        Assert.True(vm.HasBackupError);
        Assert.Contains("signed in", vm.BackupError);
    }

    [Fact]
    public async Task Fr36_use_a_recovery_key_runs_dsa_backup_key_import_with_the_key()
    {
        var cli = Cli(Report(available: false, problem: "no key", packages: 0))
            .Answer("backup key import ABCD EFGH IJKL --json", Report(packages: 4));
        var vm = await Loaded(cli);
        await vm.UseRecoveryKeyAsync("  ABCD EFGH IJKL  ");
        Assert.Contains("backup key import ABCD EFGH IJKL --json", cli.Calls);
        Assert.True(vm.BackupAvailable);
        Assert.Equal("Backup_KeyUsed", vm.BackupResult);
        Assert.True(vm.CanRestoreBackup);
    }

    [Fact]
    public async Task Fr36_an_empty_or_wrong_recovery_key_is_refused()
    {
        var cli = Cli(Report())
            .Fail("backup key import WRONG --json", "dsa: that is not a recovery key DSA made");
        var vm = await Loaded(cli);
        await vm.UseRecoveryKeyAsync("   ");
        Assert.DoesNotContain(cli.Calls, call => call.StartsWith("backup key import", StringComparison.Ordinal));
        Assert.Equal("Backup_KeyEmpty", vm.BackupError);

        await vm.UseRecoveryKeyAsync("WRONG");
        Assert.True(vm.HasBackupError);
        Assert.Contains("not a recovery key", vm.BackupError);
        Assert.False(vm.HasBackupResult);
    }
}
