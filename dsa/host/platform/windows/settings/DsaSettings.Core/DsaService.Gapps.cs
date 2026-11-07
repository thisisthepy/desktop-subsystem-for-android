using System.Text.Json;

namespace Dsa.Settings.Core;

/// <summary>Google Play: the public GApps package the service downloads and installs on the
/// user's request.</summary>
public sealed partial class DsaService
{
    /// <summary>Google's terms, shown when the service does not name the page.</summary>
    public const string DefaultGappsTermsUrl = "https://policies.google.com/terms";

    /// <summary>Google's page for registering a device it has not certified, shown when the
    /// service does not name it.</summary>
    public const string DefaultGappsRegistrationUrl = "https://www.google.com/android/uncertified/";

    /// <summary>A web address the page may open: an https one, else <paramref name="fallback"/>,
    /// so nothing else dsa prints is handed to the shell.</summary>
    private static string WebAddress(JsonElement json, string name, string fallback) =>
        OptionalString(json, name) is { } url && url.StartsWith("https://", StringComparison.OrdinalIgnoreCase)
            ? url
            : fallback;

    /// <summary>Reads what <c>dsa gapps ... --json</c> prints. A field a service from before it
    /// existed does not print reads as empty.</summary>
    public static GappsInfo ReadGapps(JsonElement json)
    {
        GappsPackageInfo? package = null;
        if (json.TryGetProperty("package", out var p) && p.ValueKind == JsonValueKind.Object)
        {
            package = new GappsPackageInfo(String(p, "variant"), String(p, "version"), String(p, "android_version"),
                Int(p, "sdk"), String(p, "arch"), String(p, "file"), String(p, "source"), String(p, "md5"));
        }
        GappsGuestInfo? android = null;
        if (json.TryGetProperty("android", out var a) && a.ValueKind == JsonValueKind.Object)
        {
            android = new GappsGuestInfo(Bool(a, "installed"), Bool(a, "active"), Int(a, "sdk"), String(a, "abi"),
                String(a, "abilist"), String(a, "device_id"));
        }
        return new GappsInfo(package, android, Bool(json, "restart_needed"),
            WebAddress(json, "terms_url", DefaultGappsTermsUrl),
            WebAddress(json, "registration_url", DefaultGappsRegistrationUrl));
    }

    /// <summary>Whether Google Play is installed, and what the running Android has.</summary>
    public async Task<GappsInfo> GappsAsync() =>
        ReadGapps(await RunJsonAsync("gapps", "status").ConfigureAwait(true));

    /// <summary>Downloads the newest package for this Android and installs it, starting Android
    /// first when it is not running. It takes minutes: the package is a few hundred MB.</summary>
    public async Task<GappsInfo> InstallGappsAsync() =>
        ReadGapps(await RunJsonAsync("gapps", "install").ConfigureAwait(true));

    /// <summary>Removes Google Play; Android goes without it from its next start.</summary>
    public async Task<GappsInfo> RemoveGappsAsync() =>
        ReadGapps(await RunJsonAsync("gapps", "remove").ConfigureAwait(true));
}

/// <summary>The GApps package the service installed, as it describes it.</summary>
public sealed record GappsPackageInfo(
    string Variant,
    string Version,
    string AndroidVersion,
    int Sdk,
    string Arch,
    string File,
    string Source,
    string Md5);

/// <summary>What the running Android says of Google Play: whether the image is on its disk,
/// whether this boot has it, and the ID Google's registration page asks for.</summary>
public sealed record GappsGuestInfo(
    bool Installed,
    bool Active,
    int Sdk,
    string Abi,
    string Abilist,
    string DeviceId);

/// <summary>Where Google Play stands for the user.</summary>
public sealed record GappsInfo(
    GappsPackageInfo? Package,
    GappsGuestInfo? Android,
    bool RestartNeeded,
    string TermsUrl,
    string RegistrationUrl);
