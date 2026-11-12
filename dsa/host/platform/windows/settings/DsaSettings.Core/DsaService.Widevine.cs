using System.Text.Json;

namespace Dsa.Settings.Core;

/// <summary>Widevine, the DRM module streaming apps play protected video with: the user's
/// computer takes it from Google's emulator image when the user asks, and dsa installs it. DSA
/// does not ship it.</summary>
public sealed partial class DsaService
{
    /// <summary>Google's terms for what its SDK repository gives: the Android SDK License.</summary>
    public const string WidevineTermsUrl = "https://developer.android.com/studio/terms";

    /// <summary>Where the emulator image Widevine comes from is downloaded.</summary>
    public const string WidevineSource = "https://dl.google.com/android/repository/sys-img/google_apis/";

    /// <summary>Reads what <c>dsa drm widevine ... --json</c> prints.</summary>
    public static WidevineInfo ReadWidevine(JsonElement json)
    {
        WidevinePackageInfo? package = null;
        if (json.TryGetProperty("package", out var p) && p.ValueKind == JsonValueKind.Object)
        {
            package = new WidevinePackageInfo(String(p, "apex"), String(p, "version"), Int(p, "android_major"),
                String(p, "abi"), String(p, "source"), String(p, "license"));
        }
        return new WidevineInfo(package, Bool(json, "on_disk"), Bool(json, "running"), Bool(json, "active"),
            Bool(json, "restart_needed"), String(json, "security_level"));
    }

    /// <summary>Whether Widevine is installed, and whether the running Android has it.</summary>
    public async Task<WidevineInfo> WidevineAsync() =>
        ReadWidevine(await RunJsonAsync("drm", "widevine", "status").ConfigureAwait(true));

    /// <summary>Downloads Google's emulator image for this Android, takes Widevine out of it
    /// unchanged and installs it. The user agreed to Google's terms in the app first, so dsa does
    /// not ask again. It takes minutes: the image is about 2 GB. Android need not run.</summary>
    public async Task<WidevineInfo> InstallWidevineAsync() =>
        ReadWidevine(await RunJsonAsync("drm", "widevine", "install", "--yes").ConfigureAwait(true));

    /// <summary>Removes Widevine, and the device certificates it kept, from Android's next
    /// start.</summary>
    public async Task<WidevineInfo> RemoveWidevineAsync() =>
        ReadWidevine(await RunJsonAsync("drm", "widevine", "remove").ConfigureAwait(true));
}

/// <summary>The Widevine module the user installed, as dsa describes it.</summary>
public sealed record WidevinePackageInfo(
    string Apex,
    string Version,
    int AndroidMajor,
    string Abi,
    string Source,
    string License);

/// <summary>Where Widevine stands for the user.</summary>
public sealed record WidevineInfo(
    WidevinePackageInfo? Package,
    bool OnDisk,
    bool Running,
    bool Active,
    bool RestartNeeded,
    string SecurityLevel);
