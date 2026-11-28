using System.Collections.ObjectModel;
using System.Globalization;
using System.Text.Json;
using System.Text.RegularExpressions;

namespace Dsa.Settings.Core;

/// <summary>What <c>dsa android version --json</c> reports.</summary>
public sealed record AndroidVersionInfo(
    string Version,
    int Major,
    string? BuildId,
    string? SecurityPatch,
    string Channel,
    IReadOnlyList<int> Supported,
    int TargetMajor,
    bool OnTrial,
    string? Notice,
    string? RestartPending = null)
{
    public static AndroidVersionInfo Parse(string json)
    {
        using var document = JsonDocument.Parse(json);
        var root = document.RootElement;
        string? Text(string name) =>
            root.TryGetProperty(name, out var value) && value.ValueKind == JsonValueKind.String ? value.GetString() : null;
        var supported = root.GetProperty("supported_majors").EnumerateArray().Select(m => m.GetInt32()).ToList();
        return new AndroidVersionInfo(
            Text("version") ?? "",
            root.GetProperty("android_major").GetInt32(),
            Text("build_id"),
            Text("security_patch"),
            Text("channel") ?? "latest",
            supported,
            root.GetProperty("target_major").GetInt32(),
            root.TryGetProperty("on_trial", out var trial) && trial.ValueKind == JsonValueKind.True,
            Text("notice"),
            Text("restart_pending") is { Length: > 0 } pending ? pending : null);
    }
}

/// <summary>One choice on the Android page: the newest version, or one major version.</summary>
public sealed record AndroidVersionChoice(string Channel, int Major, string Label);

/// <summary>How moving to a choice treats the user's data.</summary>
public enum AndroidMove
{
    /// <summary>The choice leads to the major that runs now.</summary>
    Same,
    /// <summary>A newer major: apps and their data move along.</summary>
    Up,
    /// <summary>An older major: every user's Android data is erased.</summary>
    Down,
}

/// <summary>The Android part of the Updates page: the version that runs (major, build,
/// security patch), the version it follows, where its image update stands, and the supported
/// major versions to choose from.</summary>
public sealed partial class AndroidVersionViewModel : ObservableObject
{
    private readonly IDsaCli _cli;
    private readonly ITextSource _text;
    private AndroidVersionInfo? _info;
    private string? _error;
    private bool _busy;
    private int _selected = -1;

    public AndroidVersionViewModel(IDsaCli cli, ITextSource text)
    {
        _cli = cli;
        _text = text;
    }

    public ObservableCollection<AndroidVersionChoice> Choices { get; } = new();

    public bool IsLoaded => _info is not null;

    /// <summary>Whether an Android image is installed. An installer without a bundled image
    /// records the name "none" with Android 0 until the first image has been downloaded; that is
    /// no Android version, so the page says no image is installed and leaves out the build and
    /// security patch rows instead of showing "Android 0" and "Unknown".</summary>
    public bool HasImage => _info is { Major: > 0 };

    public string VersionText => _info is null ? ""
        : HasImage ? string.Format(CultureInfo.CurrentCulture, _text.Get("Android_Major"), _info.Major)
        : _text.Get("Android_NoImage");
    public string ImageVersion => HasImage ? _info!.Version : "";
    public string BuildId => _info?.BuildId ?? _text.Get("Android_Unknown");
    public string SecurityPatch => _info?.SecurityPatch ?? _text.Get("Android_Unknown");
    public string SupportedText => _info is null ? "" : string.Format(CultureInfo.CurrentCulture,
        _text.Get("Android_Supported"), string.Join(", ", _info.Supported));

    /// <summary>The service's latest news about the image, in the app's language where the app
    /// knows the sentence.</summary>
    public string Notice => _info?.Notice is { Length: > 0 } notice ? UpdateNoticeText.Localize(_text, notice) : "";
    public bool HasNotice => !string.IsNullOrEmpty(_info?.Notice);
    /// <summary>A switch waits for its image: it is used from the next start.</summary>
    public bool IsSwitching => _info is not null && _info.TargetMajor != _info.Major;
    public string SwitchingText => _info is null ? "" : string.Format(CultureInfo.CurrentCulture,
        _text.Get("Android_Switching"), _info.TargetMajor);
    public string? Error => _error;
    public bool HasError => _error is not null;
    public bool IsBusy => _busy;

    /// <summary>The version the image updates follow: the newest supported one, or one major
    /// version and its security updates.</summary>
    public string ChannelText
    {
        get
        {
            if (_info is null)
            {
                return "";
            }
            if (_info.Channel == "latest")
            {
                var newest = _info.Supported.Count > 0 ? _info.Supported.Max() : _info.Major;
                return string.Format(CultureInfo.CurrentCulture, _text.Get("Android_FollowsLatest"), newest);
            }
            return string.Format(CultureInfo.CurrentCulture, _text.Get("Android_FollowsMajor"), _info.Channel);
        }
    }

    /// <summary>How far the image download has got, from the service's notice
    /// ("downloading Android ...: 45% ..."), or null when nothing is downloading.</summary>
    public int? DownloadPercent => DownloadPercentOf(_info?.Notice);

    public bool IsDownloading => DownloadPercent is not null;

    /// <summary>Where the Android image update stands, in one line: the download with its
    /// progress, a version waiting for the next start, the service's last news, or that none is
    /// waiting.</summary>
    public string UpdateStatusText =>
        _info is null ? ""
        : IsDownloading ? Notice
        : IsRestartPending ? string.Format(CultureInfo.CurrentCulture, _text.Get("Android_RestartPending"), RestartPending)
        : HasNotice ? Notice
        : IsSwitching ? SwitchingText
        : _text.Get("Android_UpToDate");

    /// <summary>The downloaded image the user's running Android waits to restart into, or
    /// null when none waits.</summary>
    public string? RestartPending => _info?.RestartPending;

    /// <summary>Whether the [Restart now] button is shown.</summary>
    public bool IsRestartPending => RestartPending is not null;

    /// <summary>Whether [Restart now] can be pressed now.</summary>
    public bool CanRestartNow => !_busy && IsRestartPending;

    /// <summary>Whether the service can be asked to check for an Android image now.</summary>
    public bool CanCheckNow => !_busy && _info is not null;

    /// <summary>The dsa arguments that ask for a check now: choosing the version the service
    /// already follows, which checks the signed image list at once and changes nothing else.</summary>
    public string[] CheckArguments => new[] { "android", "use", _info?.Channel ?? "latest", "--json" };

    /// <summary>The percentage in a download notice, or null.</summary>
    public static int? DownloadPercentOf(string? notice)
    {
        if (notice is null)
        {
            return null;
        }
        var match = DownloadNotice().Match(notice);
        return match.Success && int.TryParse(match.Groups[1].Value, NumberStyles.None, CultureInfo.InvariantCulture, out var percent)
            ? Math.Clamp(percent, 0, 100)
            : null;
    }

    [GeneratedRegex(@"^downloading Android [^:]*: (\d+)%")]
    private static partial Regex DownloadNotice();

    /// <summary>The choice the service keeps, or the one the user just picked.</summary>
    public int SelectedIndex
    {
        get => _selected;
        set
        {
            _selected = value;
            Changed();
            Changed(nameof(CanApply));
            Changed(nameof(SelectedMove));
        }
    }

    public AndroidVersionChoice? Selected => _selected >= 0 && _selected < Choices.Count ? Choices[_selected] : null;

    /// <summary>Whether the picked choice differs from the one the service keeps.</summary>
    public bool CanApply => !_busy && _info is not null && Selected is { } choice && choice.Channel != _info.Channel;

    public AndroidMove SelectedMove => Selected is { } choice ? MoveTo(choice) : AndroidMove.Same;

    /// <summary>How moving to <paramref name="choice"/> treats the data.</summary>
    public AndroidMove MoveTo(AndroidVersionChoice choice)
    {
        if (_info is null || choice.Major == _info.Major)
        {
            return AndroidMove.Same;
        }
        return choice.Major > _info.Major ? AndroidMove.Up : AndroidMove.Down;
    }

    /// <summary>The dsa arguments that choose <paramref name="choice"/>.</summary>
    public static string[] UseArguments(AndroidVersionChoice choice, bool eraseData) =>
        eraseData
            ? new[] { "android", "use", choice.Channel, "--erase-data", "--json" }
            : new[] { "android", "use", choice.Channel, "--json" };

    /// <summary>What the confirmation says before moving to an older major.</summary>
    public string DownWarning(AndroidVersionChoice choice) => string.Format(CultureInfo.CurrentCulture,
        _text.Get("Android_DownWarning"), choice.Major, _info?.Major ?? 0);

    public async Task LoadAsync()
    {
        await RunAsync(new[] { "android", "version", "--json" });
    }

    /// <summary>Reads the version again without showing the page as busy, for following a
    /// download while the page is shown.</summary>
    public async Task RefreshAsync()
    {
        if (_busy)
        {
            return;
        }
        try
        {
            var result = await _cli.RunAsync(new[] { "android", "version", "--json" }).ConfigureAwait(true);
            if (result.ExitCode == 0 && !_busy)
            {
                var picked = _selected;
                Show(AndroidVersionInfo.Parse(result.Output));
                if (picked >= 0 && picked < Choices.Count)
                {
                    _selected = picked;
                }
                ChangedAll();
            }
        }
        catch (Exception e) when (e is JsonException or KeyNotFoundException or InvalidOperationException)
        {
            // The page keeps what it showed; the next refresh reads again.
        }
    }

    /// <summary>Asks the service to check for an Android image now.</summary>
    public async Task CheckNowAsync()
    {
        await RunAsync(CheckArguments);
    }

    /// <summary>Chooses <paramref name="choice"/>; <paramref name="eraseData"/> is the user's
    /// agreement that an older major erases the data.</summary>
    public async Task UseAsync(AndroidVersionChoice choice, bool eraseData)
    {
        await RunAsync(UseArguments(choice, eraseData));
    }

    private static readonly string[] VersionArguments = { "android", "version", "--json" };

    /// <summary>The dsa arguments that restart Android now into the image that waits.</summary>
    public static readonly string[] RestartArguments = { "android", "restart", "--json" };

    /// <summary>Throws what the service said when the command failed.</summary>
    private static void Check(CliResult result)
    {
        if (result.ExitCode != 0)
        {
            var detail = result.Error.StartsWith("dsa: ", StringComparison.Ordinal) ? result.Error[5..] : result.Error;
            var problem = detail.Contains("cannot reach the DSA service", StringComparison.Ordinal)
                ? Problem.ServiceUnreachable
                : Problem.Refused;
            throw new DsaException(problem, detail);
        }
    }

    /// <summary>Restarts Android now into the downloaded image that waits for it, then reads
    /// the version again.</summary>
    public async Task RestartNowAsync()
    {
        await RunAsync(RestartArguments, thenRead: true);
    }

    private async Task RunAsync(string[] args, bool thenRead = false)
    {
        _busy = true;
        Changed(nameof(IsBusy));
        Changed(nameof(CanApply));
        try
        {
            var result = await _cli.RunAsync(args).ConfigureAwait(true);
            Check(result);
            if (thenRead)
            {
                result = await _cli.RunAsync(VersionArguments).ConfigureAwait(true);
                Check(result);
            }
            Show(AndroidVersionInfo.Parse(result.Output));
            _error = null;
        }
        catch (DsaException e)
        {
            _error = Format.Problem(_text, e);
        }
        catch (Exception e) when (e is JsonException or KeyNotFoundException or InvalidOperationException)
        {
            _error = Format.Problem(_text, new DsaException(Problem.Unreadable, e.Message));
        }
        finally
        {
            _busy = false;
            ChangedAll();
        }
    }

    private void Show(AndroidVersionInfo info)
    {
        _info = info;
        Choices.Clear();
        var newest = info.Supported.Count > 0 ? info.Supported.Max() : info.Major;
        Choices.Add(new AndroidVersionChoice("latest", newest, string.Format(CultureInfo.CurrentCulture,
            _text.Get("Android_ChoiceLatest"), newest)));
        foreach (var major in info.Supported.OrderByDescending(m => m))
        {
            Choices.Add(new AndroidVersionChoice(major.ToString(CultureInfo.InvariantCulture), major,
                string.Format(CultureInfo.CurrentCulture, _text.Get("Android_ChoiceMajor"), major)));
        }
        _selected = Choices.ToList().FindIndex(c => c.Channel == info.Channel);
    }
}
