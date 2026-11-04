using System.Diagnostics;
using System.Globalization;
using System.Text;
using System.Text.Json;

namespace Dsa.Settings.Core;

/// <summary>One layer of an adaptive icon: an image, a colour, or something the window does
/// not draw.</summary>
public sealed record ApkIconLayer(byte[]? Png, uint? Color);

/// <summary>An app's launcher icon: one image, or an adaptive icon's two layers.</summary>
public sealed record ApkIcon(byte[]? Png, ApkIconLayer? Background, ApkIconLayer? Foreground)
{
    public bool IsAdaptive => Png is null;
}

/// <summary>A permission an app asks for, in plain words.</summary>
public sealed record ApkPermission(string Name, string Description, bool Sensitive);

/// <summary>What the user's Android has of the app already.</summary>
public sealed record InstalledApp(long VersionCode, string VersionName);

/// <summary>What installing the APK would do, as Android would decide it.</summary>
public enum ApkInstallKind
{
    /// <summary>Not known until Android runs.</summary>
    Unknown,
    New,
    Update,
    Reinstall,
    Downgrade,
    SignatureMismatch,
}

/// <summary>What <c>dsa apk-info</c> says of an APK: the file's own information, and when
/// Android runs, what it has of the app.</summary>
public sealed record ApkDetails(
    string Package,
    string? Label,
    long VersionCode,
    string? VersionName,
    int MinSdk,
    string? MinAndroid,
    int TargetSdk,
    string? TargetAndroid,
    long Size,
    IReadOnlyList<ApkPermission> Permissions,
    ApkIcon? Icon,
    string? SignerScheme,
    string? SignerSha256,
    IReadOnlyList<string> Abis,
    bool AndroidRunning,
    InstalledApp? Installed,
    ApkInstallKind Kind)
{
    /// <summary>Reads the JSON <c>dsa apk-info --json</c> prints.</summary>
    public static ApkDetails Parse(string json)
    {
        using var document = JsonDocument.Parse(json);
        var root = document.RootElement;
        var permissions = new List<ApkPermission>();
        if (root.TryGetProperty("permissions", out var list) && list.ValueKind == JsonValueKind.Array)
        {
            foreach (var p in list.EnumerateArray())
            {
                permissions.Add(new ApkPermission(Text(p, "name") ?? "", Text(p, "description") ?? "",
                    p.TryGetProperty("sensitive", out var s) && s.ValueKind == JsonValueKind.True));
            }
        }
        var abis = new List<string>();
        if (root.TryGetProperty("abis", out var abiList) && abiList.ValueKind == JsonValueKind.Array)
        {
            abis.AddRange(abiList.EnumerateArray().Where(a => a.ValueKind == JsonValueKind.String).Select(a => a.GetString()!));
        }
        InstalledApp? installed = null;
        if (root.TryGetProperty("installed", out var i) && i.ValueKind == JsonValueKind.Object)
        {
            installed = new InstalledApp(Long(i, "version_code"), Text(i, "version_name") ?? "");
        }
        string? scheme = null, sha256 = null;
        if (root.TryGetProperty("signer", out var signer) && signer.ValueKind == JsonValueKind.Object)
        {
            scheme = Text(signer, "scheme");
            sha256 = Text(signer, "sha256");
        }
        var kind = Text(root, "install") switch
        {
            "new" => ApkInstallKind.New,
            "update" => ApkInstallKind.Update,
            "reinstall" => ApkInstallKind.Reinstall,
            "downgrade" => ApkInstallKind.Downgrade,
            "signature-mismatch" => ApkInstallKind.SignatureMismatch,
            _ => ApkInstallKind.Unknown,
        };
        return new ApkDetails(
            Text(root, "package") ?? "",
            Text(root, "label"),
            Long(root, "version_code"),
            Text(root, "version_name"),
            (int)Long(root, "min_sdk"),
            Text(root, "min_android"),
            (int)Long(root, "target_sdk"),
            Text(root, "target_android"),
            Long(root, "size"),
            permissions,
            root.TryGetProperty("icon", out var icon) ? ReadIcon(icon) : null,
            scheme,
            sha256,
            abis,
            Text(root, "android") == "running",
            installed,
            kind);
    }

    private static ApkIcon? ReadIcon(JsonElement icon)
    {
        if (icon.ValueKind != JsonValueKind.Object)
        {
            return null;
        }
        return Text(icon, "kind") switch
        {
            "image" => Bytes(icon, "png") is { } png ? new ApkIcon(png, null, null) : null,
            "adaptive" => new ApkIcon(null,
                icon.TryGetProperty("background", out var b) ? ReadLayer(b) : null,
                icon.TryGetProperty("foreground", out var f) ? ReadLayer(f) : null),
            _ => null,
        };
    }

    private static ApkIconLayer? ReadLayer(JsonElement layer)
    {
        if (layer.ValueKind != JsonValueKind.Object)
        {
            return null;
        }
        switch (Text(layer, "kind"))
        {
            case "image":
                return Bytes(layer, "png") is { } png ? new ApkIconLayer(png, null) : null;
            case "color":
                var text = Text(layer, "color")?.TrimStart('#');
                return text is not null && uint.TryParse(text, NumberStyles.HexNumber, CultureInfo.InvariantCulture, out var argb)
                    ? new ApkIconLayer(null, argb)
                    : null;
            default:
                return null;
        }
    }

    private static string? Text(JsonElement element, string name) =>
        element.TryGetProperty(name, out var value) && value.ValueKind == JsonValueKind.String ? value.GetString() : null;

    private static long Long(JsonElement element, string name) =>
        element.TryGetProperty(name, out var value) && value.ValueKind == JsonValueKind.Number && value.TryGetInt64(out var n)
            ? n
            : 0;

    private static byte[]? Bytes(JsonElement element, string name)
    {
        try
        {
            return Text(element, name) is { } text ? Convert.FromBase64String(text) : null;
        }
        catch (FormatException)
        {
            return null;
        }
    }
}

/// <summary>Runs the dsa command and hands each line it prints to <c>onLine</c> as it comes,
/// for commands that report their progress.</summary>
public interface IDsaRunner
{
    Task<CliResult> RunAsync(IReadOnlyList<string> args, Action<string>? onLine, CancellationToken cancel = default);
}

/// <summary>Runs dsa.exe as a hidden child process; cancelling ends it.</summary>
public sealed class ProcessDsaRunner : IDsaRunner
{
    private readonly string _path;

    public ProcessDsaRunner(string path)
    {
        _path = path;
    }

    public async Task<CliResult> RunAsync(IReadOnlyList<string> args, Action<string>? onLine, CancellationToken cancel = default)
    {
        var start = new ProcessStartInfo(_path)
        {
            UseShellExecute = false,
            CreateNoWindow = true,
            RedirectStandardOutput = true,
            RedirectStandardError = true,
            StandardOutputEncoding = Encoding.UTF8,
            StandardErrorEncoding = Encoding.UTF8,
        };
        foreach (var arg in args)
        {
            start.ArgumentList.Add(arg);
        }
        using var process = new Process { StartInfo = start };
        try
        {
            process.Start();
        }
        catch (Exception e) when (e is System.ComponentModel.Win32Exception or InvalidOperationException)
        {
            throw new DsaException(Problem.CliMissing, $"{_path}: {e.Message}");
        }
        using var stop = cancel.Register(() =>
        {
            try
            {
                process.Kill(true);
            }
            catch (InvalidOperationException)
            {
            }
        });
        var output = new StringBuilder();
        var error = process.StandardError.ReadToEndAsync(CancellationToken.None);
        while (await process.StandardOutput.ReadLineAsync(CancellationToken.None).ConfigureAwait(true) is { } line)
        {
            output.AppendLine(line);
            onLine?.Invoke(line);
        }
        await process.WaitForExitAsync(CancellationToken.None).ConfigureAwait(true);
        cancel.ThrowIfCancellationRequested();
        return new CliResult(process.ExitCode, output.ToString().Trim(), (await error.ConfigureAwait(true)).Trim());
    }
}
