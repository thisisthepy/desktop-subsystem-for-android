using System.Globalization;

namespace Dsa.Settings.Core;

/// <summary>What the backup section is doing now.</summary>
public enum BackupActivity
{
    None,
    BackingUp,
    Restoring,
    ShowingKey,
    UsingKey,
    Switching,
}

/// <summary>The backup section of the Advanced page: whether backup is on (off until the user
/// turns it on, since it keeps a second copy of the apps' data on this PC), where the backup of
/// Android's app data stands, backing up and restoring now, the recovery key that opens the
/// backup folder on another computer, and how Android's own keys are protected on this PC.
/// Every action goes through <c>dsa backup</c>.</summary>
public sealed partial class SettingsViewModel
{
    private BackupInfo? _backup;
    private bool _backupLoaded;
    private BackupActivity _backupActivity;
    private string? _backupError;
    private string? _backupResult;
    private string? _recoveryKey;

    public BackupInfo? Backup => _backup;

    /// <summary>Whether the service answered for the backup at all. A service from before
    /// backups, or one that cannot reach the user's backup, leaves the section saying why.</summary>
    public bool HasBackup => _backup is not null;

    /// <summary>Whether backups can be made and restored now: backup is on, and the folder and
    /// its key are there.</summary>
    public bool BackupAvailable => _backup?.Available ?? false;

    /// <summary>Whether the user turned backup on. Off until they do.</summary>
    public bool BackupEnabled => _backup?.Enabled ?? false;

    /// <summary>The switch can be used once the service has said where the backup stands.</summary>
    public bool CanToggleBackup => !_busy && !IsBackupWorking && HasBackup;

    /// <summary>What the switch's card says while backup is off: what turning it on does, and
    /// what earlier backups are still kept.</summary>
    public string BackupOffNote => _backup switch
    {
        { Enabled: false, Packages: > 0 } backup => string.Format(CultureInfo.CurrentCulture,
            _text.Get("Backup_OffKept"), backup.Packages, Format.Size(backup.Size / 1024)),
        { Enabled: false } => _text.Get("Backup_Off"),
        _ => "",
    };

    public bool HasBackupOffNote => BackupOffNote.Length > 0;

    /// <summary>Whether turning backup off leaves backups behind, which the page offers to delete.</summary>
    public bool HasBackupsKept => (_backup?.Packages ?? 0) > 0;

    /// <summary>What the page asks when backup is turned off while backups are kept.</summary>
    public string BackupDeleteQuestion => string.Format(CultureInfo.CurrentCulture,
        _text.Get("Backup_DeleteQuestion"), _backup?.Packages ?? 0, Format.Size((_backup?.Size ?? 0) / 1024));

    /// <summary>Why backups cannot be used, when they cannot. Off is not a problem.</summary>
    public string BackupProblem => _backup switch
    {
        null when _backupLoaded => _text.Get("Backup_NoService"),
        { Available: false, Problem.Length: > 0 } backup => backup.Problem,
        { Enabled: false } => "",
        { Available: false } => _text.Get("Backup_Unavailable"),
        _ => "",
    };

    /// <summary>How Android's KeyMint root key is protected, from a service that says.</summary>
    public KeyProtectionInfo? KeyProtection => _backup?.KeyMint;

    public bool HasKeyProtection => KeyProtection is not null;

    /// <summary>What protects Android's keys on this PC, in the user's language.</summary>
    public string KeyProtectionText => KeyProtection switch
    {
        null => "",
        { Kept: false } => _text.Get("KeyProtection_None"),
        { SealedBy: "tpm" } => _text.Get("KeyProtection_Tpm"),
        { SealedBy: "dpapi" } => _text.Get("KeyProtection_Dpapi"),
        { Hardware: true } key => string.Format(CultureInfo.CurrentCulture, _text.Get("KeyProtection_Hardware"), key.Description),
        { } key => string.Format(CultureInfo.CurrentCulture, _text.Get("KeyProtection_Store"), key.Description),
    };

    /// <summary>Why the PC's TPM does not seal Android's keys, when it does not.</summary>
    public string KeyProtectionDetail => KeyProtection is { Kept: true, Hardware: false, FallbackReason.Length: > 0 } key
        ? string.Format(CultureInfo.CurrentCulture, _text.Get("KeyProtection_Why"), key.FallbackReason)
        : "";

    public bool HasKeyProtectionDetail => KeyProtectionDetail.Length > 0;

    /// <summary>Android's keys are protected less than they could be: no TPM seals them.</summary>
    public bool KeyProtectionWeaker => KeyProtection is { Kept: true, Hardware: false };

    /// <summary>Why the latest start of Android got no key, and what to do.</summary>
    public string KeyProblem => KeyProtection?.Problem ?? "";

    public bool HasKeyProblem => KeyProblem.Length > 0;

    public bool HasBackupProblem => BackupProblem.Length > 0;

    /// <summary>The folder the backup is kept in, in the user's profile.</summary>
    public string BackupFolder => _backup is { Folder.Length: > 0 } backup
        ? string.Format(CultureInfo.CurrentCulture, _text.Get("Backup_Folder"), backup.Folder)
        : "";

    public bool HasBackupFolder => BackupFolder.Length > 0;

    /// <summary>When the last backup was made, in the user's time zone and format.</summary>
    public string BackupLast => string.Format(CultureInfo.CurrentCulture, _text.Get("Backup_Last"),
        When(_backup?.LastBackupUnixMs ?? 0));

    /// <summary>How many apps the backup holds, and how large it is.</summary>
    public string BackupApps => string.Format(CultureInfo.CurrentCulture, _text.Get("Backup_Apps"),
        _backup?.Packages ?? 0, Format.Size((_backup?.Size ?? 0) / 1024));

    /// <summary>What went wrong the last time a backup ran, as the service recorded it.</summary>
    public string BackupLastError => _backup is { LastError.Length: > 0 } backup
        ? string.Format(CultureInfo.CurrentCulture, _text.Get("Backup_LastError"), backup.LastError)
        : "";

    public bool HasBackupLastError => BackupLastError.Length > 0;

    public BackupActivity BackupActivity => _backupActivity;

    public bool IsBackupWorking => _backupActivity != BackupActivity.None;

    /// <summary>A backup or restore is under way; each takes minutes with many apps.</summary>
    public bool IsBackingUpOrRestoring => _backupActivity is BackupActivity.BackingUp or BackupActivity.Restoring;

    public bool CanBackUpNow => !_busy && !IsBackupWorking && BackupAvailable;

    /// <summary>Restoring is offered once the backup holds an app.</summary>
    public bool CanRestoreBackup => CanBackUpNow && (_backup?.Packages ?? 0) > 0;

    public bool CanShowRecoveryKey => !_busy && !IsBackupWorking && BackupAvailable;

    /// <summary>A recovery key can be brought whenever backup is on, also while the backup
    /// cannot be opened: bringing the key is what opens it.</summary>
    public bool CanUseRecoveryKey => !_busy && !IsBackupWorking && HasBackup && BackupEnabled;

    /// <summary>What the last action in the section failed with.</summary>
    public string BackupError => _backupError ?? "";

    public bool HasBackupError => _backupError is not null;

    /// <summary>What the last backup, restore or recovery key did.</summary>
    public string BackupResult => _backupResult ?? "";

    public bool HasBackupResult => _backupResult is not null;

    /// <summary>The recovery key, once the user asked to see it.</summary>
    public string RecoveryKey => _recoveryKey ?? "";

    public bool HasRecoveryKey => _recoveryKey is not null;

    /// <summary>The button that shows the key is there while the key is not shown.</summary>
    public bool ShowsRecoveryKeyButton => !HasRecoveryKey;

    /// <summary>Tells the user to write the key down and keep it apart from this computer.</summary>
    public string RecoveryKeyAdvice => _text.Get("Backup_KeyAdvice");

    /// <summary>A time in Unix milliseconds in <paramref name="zone"/>, in the format of
    /// <paramref name="culture"/>; 0 or less is never.</summary>
    public static string BackupTime(long unixMs, TimeZoneInfo zone, CultureInfo culture, string never) =>
        unixMs <= 0
            ? never
            : TimeZoneInfo.ConvertTime(DateTimeOffset.FromUnixTimeMilliseconds(unixMs), zone)
                .ToString("g", culture);

    private string When(long unixMs) =>
        BackupTime(unixMs, TimeZoneInfo.Local, CultureInfo.CurrentCulture, _text.Get("Backup_Never"));

    /// <summary>Reads where the backup stands. A service that cannot say leaves the section
    /// saying backups are not there.</summary>
    public async Task LoadBackupAsync()
    {
        try
        {
            _backup = await _service.BackupAsync().ConfigureAwait(true);
        }
        catch (DsaException)
        {
            _backup = null;
        }
        _backupLoaded = true;
        ChangedAll();
    }

    /// <summary>Turns backup on or off. The page asks first, when backups are kept, whether
    /// turning it off deletes them (<paramref name="deleteBackups"/>); this does not ask again.</summary>
    public Task SetBackupAsync(bool on, bool deleteBackups = false) => RunBackupAsync(BackupActivity.Switching, async () =>
    {
        var report = await _service.SetBackupAsync(on, !on && deleteBackups).ConfigureAwait(true);
        _backup = report;
        _recoveryKey = null;
        _backupResult = on
            ? _text.Get("Backup_TurnedOn")
            : deleteBackups ? _text.Get("Backup_TurnedOffDeleted") : _text.Get("Backup_TurnedOff");
    });

    /// <summary>Backs up every app's data now.</summary>
    public Task BackUpNowAsync() => RunBackupAsync(BackupActivity.BackingUp, async () =>
    {
        var report = await _service.BackUpNowAsync().ConfigureAwait(true);
        _backup = report;
        _backupResult = report.Failed > 0
            ? string.Format(CultureInfo.CurrentCulture, _text.Get("Backup_DoneSome"), report.Succeeded, report.Failed)
            : string.Format(CultureInfo.CurrentCulture, _text.Get("Backup_Done"), report.Succeeded);
    });

    /// <summary>Restores every app in the backup. The page asks the user first, with
    /// <see cref="RestoreWarning"/>; this does not ask again.</summary>
    public Task RestoreBackupAsync() => RunBackupAsync(BackupActivity.Restoring, async () =>
    {
        var report = await _service.RestoreBackupAsync().ConfigureAwait(true);
        _backup = report;
        _backupResult = report.Failed > 0
            ? string.Format(CultureInfo.CurrentCulture, _text.Get("Backup_RestoredSome"), report.Succeeded,
                report.Installed, report.Failed)
            : string.Format(CultureInfo.CurrentCulture, _text.Get("Backup_Restored"), report.Succeeded,
                report.Installed);
    });

    /// <summary>What the confirmation of a restore says: each app's data is replaced by the
    /// backup's.</summary>
    public string RestoreWarning => _text.Get("Backup_RestoreWarning");

    /// <summary>Shows the recovery key, with the advice to write it down.</summary>
    public Task ShowRecoveryKeyAsync() => RunBackupAsync(BackupActivity.ShowingKey, async () =>
    {
        var key = await _service.BackupRecoveryKeyAsync().ConfigureAwait(true);
        if (key.Length == 0)
        {
            throw new DsaException(Problem.Unreadable, "dsa printed no recovery key");
        }
        _recoveryKey = key;
    }, clearsResult: false);

    /// <summary>Hides the recovery key again.</summary>
    public void HideRecoveryKey()
    {
        _recoveryKey = null;
        ChangedAll();
    }

    /// <summary>Keeps the key <paramref name="recoveryKey"/> carries as this user's backup key.
    /// An empty entry is not sent.</summary>
    public Task UseRecoveryKeyAsync(string recoveryKey)
    {
        var key = recoveryKey.Trim();
        if (key.Length == 0)
        {
            _backupError = _text.Get("Backup_KeyEmpty");
            ChangedAll();
            return Task.CompletedTask;
        }
        return RunBackupAsync(BackupActivity.UsingKey, async () =>
        {
            _backup = await _service.UseBackupRecoveryKeyAsync(key).ConfigureAwait(true);
            _recoveryKey = null;
            _backupResult = _text.Get("Backup_KeyUsed");
        });
    }

    private async Task RunBackupAsync(BackupActivity activity, Func<Task> work, bool clearsResult = true)
    {
        _backupActivity = activity;
        _backupError = null;
        if (clearsResult)
        {
            _backupResult = null;
        }
        ChangedAll();
        try
        {
            await work().ConfigureAwait(true);
        }
        catch (Exception e)
        {
            _backupError = Format.Problem(_text, e);
        }
        finally
        {
            _backupActivity = BackupActivity.None;
            ChangedAll();
        }
    }
}
