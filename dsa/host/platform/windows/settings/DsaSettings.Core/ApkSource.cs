namespace Dsa.Settings.Core;

/// <summary>Where an APK came from, as the installer window's Source line says it: the host it
/// was downloaded from, which browsers record in the file's Zone.Identifier stream, or else the
/// folder the file is in.</summary>
public static class ApkSource
{
    /// <summary>Where the APK at <paramref name="path"/> came from.</summary>
    public static string Of(string path) => Describe(path, ReadZoneIdentifier(path));

    /// <summary>The host of the HostUrl in <paramref name="zoneIdentifier"/>, else of its
    /// ReferrerUrl, else the folder of <paramref name="path"/>. A value without a host, such as
    /// the <c>about:internet</c> some browsers write, says nothing about the source.</summary>
    public static string Describe(string path, string? zoneIdentifier) =>
        Host(zoneIdentifier, "HostUrl")
        ?? Host(zoneIdentifier, "ReferrerUrl")
        ?? Path.GetDirectoryName(Path.GetFullPath(path))
        ?? path;

    /// <summary>The file's Zone.Identifier stream, or null when it has none or it cannot be
    /// read: files from a disk or a volume without streams have none, and that is not a fault.</summary>
    private static string? ReadZoneIdentifier(string path)
    {
        try
        {
            return File.ReadAllText(path + ":Zone.Identifier");
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException or NotSupportedException
            or ArgumentException or System.Security.SecurityException)
        {
            return null;
        }
    }

    private static string? Host(string? zoneIdentifier, string key)
    {
        if (zoneIdentifier is null)
        {
            return null;
        }
        foreach (var line in zoneIdentifier.Split('\n'))
        {
            var at = line.IndexOf('=');
            if (at > 0 && line[..at].Trim().Equals(key, StringComparison.OrdinalIgnoreCase)
                && Uri.TryCreate(line[(at + 1)..].Trim(), UriKind.Absolute, out var url)
                && url.Host.Length > 0)
            {
                return url.Host;
            }
        }
        return null;
    }
}
