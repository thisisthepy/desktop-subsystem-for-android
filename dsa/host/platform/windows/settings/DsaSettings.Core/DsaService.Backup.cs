using System.Text.Json;

namespace Dsa.Settings.Core;

/// <summary>How the root key of Android's KeyMint is sealed on this PC, as <c>dsa backup
/// --json</c> prints it under <c>keymint_key</c>. <see cref="SealedBy"/> is <c>tpm</c>,
/// <c>dpapi</c> (and on other hosts <c>secure-enclave</c>, <c>secret-service</c>,
/// <c>keychain</c>); <see cref="Kept"/> is false until Android first starts.</summary>
public sealed record KeyProtectionInfo(
    bool Kept,
    string SealedBy,
    bool Hardware,
    string Description,
    string FallbackReason,
    string Problem);

/// <summary>Where the backup of Android's app data stands, as <c>dsa backup --json</c> prints
/// it. Times are Unix milliseconds, 0 for never. <see cref="Succeeded"/>, <see cref="Failed"/>
/// and <see cref="Installed"/> count what a backup or restore just run covered.
/// <see cref="Enabled"/> is whether the user turned backup on; it is off until they do.</summary>
public sealed record BackupInfo(
    bool Available,
    string Problem,
    string Folder,
    long LastBackupUnixMs,
    long LastRestoreUnixMs,
    ulong Packages,
    ulong Size,
    string LastError,
    ulong Succeeded,
    ulong Failed,
    ulong Installed,
    bool Enabled = false,
    KeyProtectionInfo? KeyMint = null);

/// <summary>The backup of Android's app data: encrypted in the user's DSA Backup folder, with a
/// key Windows keeps for the user, made only once the user turns backup on.</summary>
public sealed partial class DsaService
{
    private static long Long(JsonElement element, string name) =>
        element.TryGetProperty(name, out var value) && value.ValueKind == JsonValueKind.Number
            && value.TryGetInt64(out var number)
            ? number
            : 0;

    /// <summary>Reads <c>keymint_key</c>; null from a service that does not print it.</summary>
    public static KeyProtectionInfo? ReadKeyProtection(JsonElement json) =>
        json.TryGetProperty("keymint_key", out var key) && key.ValueKind == JsonValueKind.Object
            ? new KeyProtectionInfo(
                Bool(key, "kept"),
                String(key, "sealed_by"),
                Bool(key, "hardware"),
                String(key, "description"),
                String(key, "fallback_reason"),
                String(key, "problem"))
            : null;

    /// <summary>Reads what <c>dsa backup ... --json</c> prints. A field a service from before it
    /// existed does not print reads as empty.</summary>
    public static BackupInfo ReadBackup(JsonElement json) => new(
        Bool(json, "available"),
        String(json, "problem"),
        String(json, "folder"),
        Long(json, "last_backup_unix_ms"),
        Long(json, "last_restore_unix_ms"),
        Number(json, "packages"),
        Number(json, "size"),
        String(json, "last_error"),
        Number(json, "succeeded"),
        Number(json, "failed"),
        Number(json, "installed"),
        Bool(json, "enabled"),
        ReadKeyProtection(json));

    /// <summary>Where the backup stands. It never starts Android.</summary>
    public async Task<BackupInfo> BackupAsync() =>
        ReadBackup(await RunJsonAsync("backup", "status").ConfigureAwait(true));

    /// <summary>Turns backup on, or off; off with <paramref name="deleteBackups"/>, the backups
    /// kept so far and their key are removed as well.</summary>
    public async Task<BackupInfo> SetBackupAsync(bool on, bool deleteBackups) =>
        ReadBackup(await (on
                ? RunJsonAsync("backup", "on")
                : deleteBackups
                    ? RunJsonAsync("backup", "off", "--delete-backups")
                    : RunJsonAsync("backup", "off"))
            .ConfigureAwait(true));

    /// <summary>Backs up every app's data now, starting Android first when it is off.</summary>
    public async Task<BackupInfo> BackUpNowAsync() =>
        ReadBackup(await RunJsonAsync("backup", "now").ConfigureAwait(true));

    /// <summary>Restores every app in the backup, installing the apps that are missing, starting
    /// Android first when it is off.</summary>
    public async Task<BackupInfo> RestoreBackupAsync() =>
        ReadBackup(await RunJsonAsync("backup", "restore").ConfigureAwait(true));

    /// <summary>The recovery key: the backup key written as words, which opens the backup folder
    /// on another computer.</summary>
    public async Task<string> BackupRecoveryKeyAsync() =>
        String(await RunJsonAsync("backup", "key", "show").ConfigureAwait(true), "recovery_key");

    /// <summary>Keeps the key <paramref name="recoveryKey"/> carries as this user's backup key,
    /// so the backup folder made with it can be restored here.</summary>
    public async Task<BackupInfo> UseBackupRecoveryKeyAsync(string recoveryKey) =>
        ReadBackup(await RunJsonAsync("backup", "key", "import", recoveryKey).ConfigureAwait(true));
}
