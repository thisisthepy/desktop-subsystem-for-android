namespace Dsa.Settings.Core;

/// <summary>Where DSA keeps its files, as the Windows host's paths module lays them out: the
/// machine's folder (<c>%ProgramData%\DSA</c>, or the data folder chosen at install) for the
/// service and Android, and each user's <c>%LOCALAPPDATA%\DSA</c> for what runs in their
/// session. The app only reads these to open them; it writes nothing there.</summary>
public static class StoragePaths
{
    /// <summary>This user's DSA folder.</summary>
    public static string UserRoot => Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "DSA");

    /// <summary>The logs of this user's window manager and app windows.</summary>
    public static string UserLogs => Path.Combine(UserRoot, "logs");

    /// <summary>The machine's DSA folder when the installer did not record another.</summary>
    public static string DefaultMachineRoot => Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.CommonApplicationData), "DSA");

    /// <summary>The logs of the service and of each user's Android: the folder the service
    /// reports, or else the <c>logs</c> folder of the machine's DSA folder
    /// (<paramref name="machineRoot"/>, the installer's data folder when known).</summary>
    public static string MachineLogs(string? reported, string? machineRoot = null)
    {
        if (!string.IsNullOrEmpty(reported))
        {
            return reported;
        }
        var root = string.IsNullOrEmpty(machineRoot) ? DefaultMachineRoot : machineRoot;
        return Path.Combine(root, "logs");
    }
}
