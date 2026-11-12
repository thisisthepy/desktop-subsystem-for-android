using System.IO.Compression;
using System.Text.RegularExpressions;

namespace Dsa.Settings.Core;

/// <summary>What a file the APK Installer cannot install is.</summary>
public enum UnsupportedKind
{
    /// <summary>An app split into several APKs and packed together (<c>.xapk</c>, <c>.apks</c>
    /// or <c>.apkm</c>) that is damaged, or is not one app.</summary>
    SplitBundle,
    /// <summary>An Android App Bundle (<c>.aab</c>), the format apps are uploaded to stores in.</summary>
    AppBundle,
    /// <summary>A bundle whose APKs are encrypted, as some older <c>.apkm</c> files are.</summary>
    EncryptedBundle,
    /// <summary>A bundle with a split signed with another certificate than its base APK.</summary>
    SplitSignatureMismatch,
    /// <summary>An app whose native code is none DSA's Android runs, which is 64-bit only: it
    /// has 32-bit code only, or is a build made to run as 32-bit code.</summary>
    UnsupportedAbi,
    /// <summary>Anything else that is not an APK.</summary>
    NotApk,
}

/// <summary>
/// Says in plain words why a file cannot be installed. dsa names the reason with a stable
/// code when it refuses a bundle; otherwise what the file is comes from what is in it where it
/// can be read, so a renamed bundle is still named for what it is, and from its extension
/// otherwise. The window is English in every language, so these are not in the string tables.
/// </summary>
public static class UnsupportedFile
{
    private static readonly Regex Refusal = new(@"^(?:dsa: )?not installed \(([a-z-]+)\): [^\r\n]*?: (.*)$", RegexOptions.Singleline);

    public static UnsupportedKind Classify(string path)
    {
        if (FromContent(path) is { } kind)
        {
            return kind;
        }
        return System.IO.Path.GetExtension(path).ToLowerInvariant() switch
        {
            ".xapk" or ".apks" or ".apkm" => UnsupportedKind.SplitBundle,
            ".aab" => UnsupportedKind.AppBundle,
            _ => UnsupportedKind.NotApk,
        };
    }

    /// <summary>What dsa's refusal of a file before Android saw it says it is, and dsa's own
    /// words on why; null when the line is no such refusal.</summary>
    public static (UnsupportedKind Kind, string Detail)? FromRefusal(string error)
    {
        var match = Refusal.Match(error.Trim());
        if (!match.Success)
        {
            return null;
        }
        UnsupportedKind? kind = match.Groups[1].Value switch
        {
            "app-bundle" => UnsupportedKind.AppBundle,
            "encrypted-bundle" => UnsupportedKind.EncryptedBundle,
            "split-signature-mismatch" => UnsupportedKind.SplitSignatureMismatch,
            "invalid-bundle" => UnsupportedKind.SplitBundle,
            "unsupported-abi" => UnsupportedKind.UnsupportedAbi,
            _ => null,
        };
        return kind is { } known ? (known, match.Groups[2].Value.Trim()) : null;
    }

    /// <summary>What the archive holds says what it is: an App Bundle has its BundleConfig.pb
    /// and its base module; a split bundle holds APKs, with toc.pb (.apks), info.json (.apkm)
    /// or manifest.json (.xapk) beside them. Null when the file is not an archive that says.</summary>
    private static UnsupportedKind? FromContent(string path)
    {
        try
        {
            using var zip = ZipFile.OpenRead(path);
            var names = zip.Entries.Select(e => e.FullName.Replace('\\', '/')).ToList();
            if (names.Any(n => n == "BundleConfig.pb" || n == "base/manifest/AndroidManifest.xml"))
            {
                return UnsupportedKind.AppBundle;
            }
            var bundled = names.Any(n => n.EndsWith(".apk", StringComparison.OrdinalIgnoreCase));
            return bundled || names.Contains("toc.pb") ? UnsupportedKind.SplitBundle : null;
        }
        catch (Exception e) when (e is InvalidDataException or IOException or UnauthorizedAccessException
            or NotSupportedException or ArgumentException)
        {
            return null;
        }
    }

    /// <summary>The sentence the window shows for <paramref name="fileName"/>. A damaged
    /// bundle's sentence ends with what is wrong with it, when dsa said.</summary>
    public static string Message(UnsupportedKind kind, string fileName, string? detail = null) => kind switch
    {
        UnsupportedKind.SplitBundle when !string.IsNullOrWhiteSpace(detail) =>
            $"{fileName} is a split APK bundle that cannot be installed: {detail.TrimEnd('.')}.",
        UnsupportedKind.SplitBundle =>
            $"{fileName} is a split APK bundle that cannot be read, so it cannot be installed.",
        UnsupportedKind.AppBundle =>
            $"{fileName} is an Android App Bundle. App Bundles are a publishing format for app stores and cannot be installed as they are: making APKs of one means signing them with another key, and the app would then no longer take updates from where it came from. Get the app as an APK, or as an .xapk, .apks or .apkm bundle.",
        UnsupportedKind.EncryptedBundle =>
            $"{fileName} is an encrypted bundle, so its APKs cannot be read. Some older .apkm files are, and only the app that made them opens them. Get the app as an APK, or as a newer bundle.",
        UnsupportedKind.SplitSignatureMismatch =>
            $"{fileName} holds a split APK signed with another certificate than its base APK, so the bundle was changed after it was made. Android would refuse it.",
        UnsupportedKind.UnsupportedAbi when !string.IsNullOrWhiteSpace(detail) =>
            $"{fileName} cannot run in DSA: {detail.TrimEnd('.')}.",
        UnsupportedKind.UnsupportedAbi =>
            $"{fileName} cannot run in DSA: its native code is none that DSA's Android runs, which runs 64-bit code only. Get the app's x86_64 or arm64-v8a build.",
        _ => $"{fileName} is not a valid APK file.",
    };
}
