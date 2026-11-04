using System.Globalization;
using System.Text.Json;
using System.Text.RegularExpressions;

namespace Dsa.Settings.Core;

/// <summary>The screens of the APK install window, as setup's screens are laid out.</summary>
public enum ApkInstallPage
{
    /// <summary>Reading the APK.</summary>
    Reading,
    /// <summary>What the APK is, and the button that installs it.</summary>
    Details,
    /// <summary>Starting Android, copying the APK, Android installing it.</summary>
    Progress,
    Done,
    Failed,
}

/// <summary>One line of the details card: what it is, and its value.</summary>
public sealed record ApkFact(string Name, string Value);

/// <summary>A permission as the window lists it.</summary>
public sealed record ApkPermissionRow(string Description, string Name, bool Sensitive);

/// <summary>
/// The APK install window: opened on an APK (from Explorer, or dropped on the settings app),
/// it shows what the file says about the app before Android is started, then installs it.
/// Installing starts Android first when it is not running, then checks again what Android has
/// of the app: an update shows both versions and waits for the user, and a downgrade or an APK
/// signed with another key is refused with the reason, as Android would refuse it.
/// </summary>
public sealed class ApkInstallViewModel : ObservableObject
{
    private static readonly Regex Refusal = new(@"^(?:dsa: )?not installed \(([a-z-]+)\): (.*)$", RegexOptions.Singleline);

    private readonly IDsaRunner _dsa;
    private readonly ITextSource _text;
    private CancellationTokenSource? _cancel;
    private ApkDetails? _apk;
    /// <summary>The install kind the user saw when they pressed the button.</summary>
    private ApkInstallKind _confirmed = ApkInstallKind.Unknown;

    public ApkInstallViewModel(IDsaRunner dsa, ITextSource text, string path)
    {
        _dsa = dsa;
        _text = text;
        Path = path;
        FileName = System.IO.Path.GetFileName(path);
        Title = FileName;
    }

    public string Path { get; }
    public string FileName { get; }
    public ApkInstallPage Page { get; private set; } = ApkInstallPage.Reading;
    public ApkDetails? Apk => _apk;
    public string Title { get; private set; }
    public string Subtitle { get; private set; } = "";
    public string VersionChip { get; private set; } = "";
    public string SizeChip { get; private set; } = "";
    public string AndroidChip { get; private set; } = "";
    /// <summary>What installing does: a new install, an update from one version to another, or
    /// why it cannot be done.</summary>
    public string Summary { get; private set; } = "";
    public ApkInstallKind Kind => _apk?.Kind ?? ApkInstallKind.Unknown;
    /// <summary>Whether Android would refuse the APK, so the install button is off.</summary>
    public bool IsBlocked => Kind is ApkInstallKind.Downgrade or ApkInstallKind.SignatureMismatch;
    public bool CanInstall => Page == ApkInstallPage.Details && _apk is not null && !IsBlocked;
    public string InstallButton => _text.Get(Kind switch
    {
        ApkInstallKind.Update => "ApkWindow_Update",
        ApkInstallKind.Reinstall => "ApkWindow_Reinstall",
        _ => "ApkWindow_Install",
    });
    public IReadOnlyList<ApkFact> Facts { get; private set; } = Array.Empty<ApkFact>();
    public IReadOnlyList<ApkPermissionRow> Permissions { get; private set; } = Array.Empty<ApkPermissionRow>();
    public string PermissionsHeader { get; private set; } = "";
    public string ProgressTitle { get; private set; } = "";
    public string ProgressDetail { get; private set; } = "";
    /// <summary>How much of the APK has reached Android; null while the step has no measure.</summary>
    public int? ProgressPercent { get; private set; }
    public bool CanCancel => Page is ApkInstallPage.Details or ApkInstallPage.Progress && !_cancelling;
    public string ResultTitle { get; private set; } = "";
    public string ResultText { get; private set; } = "";
    public bool CanOpen => Page == ApkInstallPage.Done && _apk is not null;

    private bool _cancelling;

    private string F(string key, params object?[] args) =>
        string.Format(CultureInfo.CurrentCulture, _text.Get(key), args);

    private async Task<CliResult> Run(IReadOnlyList<string> args, Action<string>? onLine = null) =>
        await _dsa.RunAsync(args, onLine, _cancel?.Token ?? CancellationToken.None).ConfigureAwait(true);

    private static string Detail(CliResult result) =>
        result.Error.StartsWith("dsa: ", StringComparison.Ordinal) ? result.Error["dsa: ".Length..] : result.Error;

    /// <summary>Reads the APK. Android is not started for it.</summary>
    public async Task LoadAsync()
    {
        Page = ApkInstallPage.Reading;
        ChangedAll();
        try
        {
            var result = await Run(new[] { "apk-info", Path, "--json" }).ConfigureAwait(true);
            if (result.ExitCode != 0)
            {
                Fail(_text.Get("ApkWindow_NotApkTitle"), F("ApkWindow_NotApkText", FileName, Detail(result)));
                return;
            }
            Show(ApkDetails.Parse(result.Output));
        }
        catch (Exception e) when (e is DsaException or JsonException)
        {
            Fail(_text.Get("ApkWindow_NotApkTitle"), F("ApkWindow_NotApkText", FileName,
                e is DsaException ? Format.Problem(_text, e) : e.Message));
        }
    }

    private void Show(ApkDetails apk)
    {
        _apk = apk;
        Title = string.IsNullOrWhiteSpace(apk.Label) ? apk.Package : apk.Label!;
        Subtitle = apk.Package;
        VersionChip = F("ApkWindow_VersionChip", apk.VersionName ?? apk.VersionCode.ToString(CultureInfo.CurrentCulture));
        SizeChip = Size(apk.Size);
        AndroidChip = F("ApkWindow_AndroidChip", apk.MinAndroid ?? apk.MinSdk.ToString(CultureInfo.CurrentCulture));
        var version = VersionText(apk.VersionName, apk.VersionCode);
        Summary = apk.Kind switch
        {
            ApkInstallKind.New => F("ApkWindow_SummaryNew", Title),
            ApkInstallKind.Update => F("ApkWindow_SummaryUpdate", Title,
                VersionText(apk.Installed?.VersionName, apk.Installed?.VersionCode ?? 0), version),
            ApkInstallKind.Reinstall => F("ApkWindow_SummaryReinstall", Title, version),
            ApkInstallKind.Downgrade => F("ApkWindow_SummaryDowngrade", Title,
                VersionText(apk.Installed?.VersionName, apk.Installed?.VersionCode ?? 0), version),
            ApkInstallKind.SignatureMismatch => F("ApkWindow_SummarySignature", Title),
            _ => F("ApkWindow_SummaryUnknown", Title),
        };
        var facts = new List<ApkFact>
        {
            new(_text.Get("ApkWindow_FactPackage"), apk.Package),
            new(_text.Get("ApkWindow_FactVersion"), version),
        };
        if (apk.Installed is { } installed)
        {
            facts.Add(new(_text.Get("ApkWindow_FactInstalled"), VersionText(installed.VersionName, installed.VersionCode)));
        }
        facts.Add(new(_text.Get("ApkWindow_FactAndroid"), F("ApkWindow_AndroidRange",
            AndroidText(apk.MinAndroid, apk.MinSdk), AndroidText(apk.TargetAndroid, apk.TargetSdk))));
        facts.Add(new(_text.Get("ApkWindow_FactSize"), Size(apk.Size)));
        if (apk.Abis.Count > 0)
        {
            facts.Add(new(_text.Get("ApkWindow_FactAbis"), string.Join(", ", apk.Abis)));
        }
        facts.Add(new(_text.Get("ApkWindow_FactSigner"), apk.SignerSha256 is { } sha
            ? F("ApkWindow_Signer", apk.SignerScheme ?? "", sha)
            : _text.Get("ApkWindow_Unsigned")));
        Facts = facts;
        Permissions = apk.Permissions
            .OrderByDescending(p => p.Sensitive)
            .Select(p => new ApkPermissionRow(p.Description, p.Name, p.Sensitive))
            .ToList();
        PermissionsHeader = apk.Permissions.Count == 0
            ? _text.Get("ApkWindow_NoPermissions")
            : F("ApkWindow_PermissionsHeader", apk.Permissions.Count);
        Page = ApkInstallPage.Details;
        ChangedAll();
    }

    private string VersionText(string? name, long code) =>
        string.IsNullOrEmpty(name)
            ? code.ToString(CultureInfo.CurrentCulture)
            : F("ApkWindow_VersionText", name, code);

    private string AndroidText(string? version, int sdk) =>
        version is null ? F("ApkWindow_ApiLevel", sdk) : F("ApkWindow_AndroidVersion", version, sdk);

    /// <summary>A file size in KB below 1 MB, MB below 1 GB and GB above, in the user's number format.</summary>
    public static string Size(long bytes)
    {
        var culture = CultureInfo.CurrentCulture;
        return bytes switch
        {
            < 1024 * 1024 => string.Format(culture, "{0:0.#} KB", bytes / 1024.0),
            < 1024L * 1024 * 1024 => string.Format(culture, "{0:0.#} MB", bytes / (1024.0 * 1024)),
            _ => string.Format(culture, "{0:0.##} GB", bytes / (1024.0 * 1024 * 1024)),
        };
    }

    private void Progress(string title, string detail, int? percent)
    {
        Page = ApkInstallPage.Progress;
        ProgressTitle = title;
        ProgressDetail = detail;
        ProgressPercent = percent;
        ChangedAll();
    }

    private void Fail(string title, string text)
    {
        Page = ApkInstallPage.Failed;
        ResultTitle = title;
        ResultText = text;
        ChangedAll();
    }

    /// <summary>Installs the APK: starts Android when it is not running, checks what it has of
    /// the app, and sends the APK. An update the user has not seen yet, and an APK Android
    /// would refuse, go back to the details instead.</summary>
    public async Task InstallAsync()
    {
        if (!CanInstall || _apk is null)
        {
            return;
        }
        var apk = _apk;
        _confirmed = apk.Kind;
        _cancel = new CancellationTokenSource();
        _cancelling = false;
        try
        {
            if (!apk.AndroidRunning)
            {
                Progress(_text.Get("ApkWindow_StartingTitle"), _text.Get("ApkWindow_StartingDetail"), null);
                var started = await Run(new[] { "start", "--wait", "--json" }).ConfigureAwait(true);
                if (started.ExitCode != 0)
                {
                    Fail(_text.Get("ApkWindow_FailedTitle"), F("ApkWindow_StartFailed", Detail(started)));
                    return;
                }
                Progress(_text.Get("ApkWindow_CheckingTitle"), _text.Get("ApkWindow_CheckingDetail"), null);
                var checkedAgain = await Run(new[] { "apk-info", Path, "--json" }).ConfigureAwait(true);
                if (checkedAgain.ExitCode != 0)
                {
                    Fail(_text.Get("ApkWindow_FailedTitle"), Detail(checkedAgain));
                    return;
                }
                var now = ApkDetails.Parse(checkedAgain.Output);
                // What the user agreed to was a new install; anything else is theirs to see
                // first, as Android would ask before replacing an app.
                if (now.Kind != ApkInstallKind.New && now.Kind != _confirmed)
                {
                    Show(now);
                    return;
                }
                apk = now;
                _apk = now;
            }
            await SendAsync(apk).ConfigureAwait(true);
        }
        catch (OperationCanceledException)
        {
            Fail(_text.Get("ApkWindow_FailedTitle"), _text.Get("ApkWindow_Cancelled"));
        }
        catch (DsaException e)
        {
            Fail(_text.Get("ApkWindow_FailedTitle"), Format.Problem(_text, e));
        }
        catch (JsonException e)
        {
            Fail(_text.Get("ApkWindow_FailedTitle"), F("ApkWindow_NotSent", e.Message));
        }
        finally
        {
            _cancel?.Dispose();
            _cancel = null;
            Changed(nameof(CanCancel));
        }
    }

    private async Task SendAsync(ApkDetails apk)
    {
        var title = F("ApkWindow_InstallingTitle", Title);
        Progress(title, _text.Get("ApkWindow_SendingDetail"), 0);
        var result = await Run(new[] { "install", Path, "--progress", "--json" }, line =>
        {
            if (line.StartsWith("progress ", StringComparison.Ordinal) &&
                int.TryParse(line["progress ".Length..], NumberStyles.Integer, CultureInfo.InvariantCulture, out var percent))
            {
                Progress(title, _text.Get("ApkWindow_SendingDetail"), Math.Clamp(percent, 0, 100));
            }
            else if (line == "stage starting")
            {
                Progress(_text.Get("ApkWindow_StartingTitle"), _text.Get("ApkWindow_StartingDetail"), null);
            }
            else if (line == "stage sending")
            {
                Progress(title, _text.Get("ApkWindow_SendingDetail"), 0);
            }
            else if (line == "stage installing")
            {
                _cancelling = true;
                Progress(title, _text.Get("ApkWindow_AndroidInstallingDetail"), null);
            }
        }).ConfigureAwait(true);
        if (result.ExitCode == 0)
        {
            Page = ApkInstallPage.Done;
            ResultTitle = F(apk.Kind == ApkInstallKind.Update ? "ApkWindow_UpdatedTitle" : "ApkWindow_DoneTitle", Title);
            ResultText = _text.Get("ApkWindow_DoneText");
            ChangedAll();
            return;
        }
        var detail = Detail(result);
        var refused = Refusal.Match(detail);
        if (refused.Success)
        {
            var reason = _text.Get($"ApkWindow_Reason_{refused.Groups[1].Value}");
            var words = AndroidWords(refused.Groups[2].Value);
            Fail(_text.Get("ApkWindow_FailedTitle"),
                words is null ? reason : F("ApkWindow_ReasonWithWords", reason, words));
            return;
        }
        Fail(_text.Get("ApkWindow_FailedTitle"), F("ApkWindow_NotSent", detail.Length > 0 ? detail : $"dsa ended with code {result.ExitCode}"));
    }

    /// <summary>Android's own words in dsa's refusal, after "Android said: ".</summary>
    private static string? AndroidWords(string detail)
    {
        const string said = "Android said: ";
        var at = detail.IndexOf(said, StringComparison.Ordinal);
        return at < 0 ? null : detail[(at + said.Length)..].Trim();
    }

    /// <summary>Stops an install under way: the APK stops going to Android, which drops what
    /// it had. Once Android has all of it, the install cannot be stopped.</summary>
    public void Cancel()
    {
        if (_cancel is { } cancel && !_cancelling)
        {
            _cancelling = true;
            ProgressDetail = _text.Get("ApkWindow_Cancelling");
            ChangedAll();
            cancel.Cancel();
        }
    }

    /// <summary>Whether an install is under way, so closing the window would stop it.</summary>
    public bool IsBusy => Page == ApkInstallPage.Progress;

    /// <summary>Opens the installed app in its window. Returns why it could not, or null.</summary>
    public async Task<string?> OpenAsync()
    {
        if (!CanOpen || _apk is null)
        {
            return null;
        }
        try
        {
            var result = await _dsa.RunAsync(new[] { "launch", _apk.Package }, null).ConfigureAwait(true);
            return result.ExitCode == 0 ? null : F("ApkWindow_OpenFailed", Title, Detail(result));
        }
        catch (DsaException e)
        {
            return F("ApkWindow_OpenFailed", Title, Format.Problem(_text, e));
        }
    }
}
