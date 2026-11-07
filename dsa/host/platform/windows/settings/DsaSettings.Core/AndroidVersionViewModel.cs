using System.Collections.ObjectModel;
using System.Globalization;
using System.Text.Json;

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
    string? Notice)
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
            Text("notice"));
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

/// <summary>The Android page: the version that runs (major, build, security patch) and the
/// supported major versions to choose from.</summary>
public sealed class AndroidVersionViewModel : ObservableObject
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
    public string VersionText => _info is null ? "" : string.Format(CultureInfo.CurrentCulture,
        _text.Get("Android_Major"), _info.Major);
    public string ImageVersion => _info?.Version ?? "";
    public string BuildId => _info?.BuildId ?? _text.Get("Android_Unknown");
    public string SecurityPatch => _info?.SecurityPatch ?? _text.Get("Android_Unknown");
    public string SupportedText => _info is null ? "" : string.Format(CultureInfo.CurrentCulture,
        _text.Get("Android_Supported"), string.Join(", ", _info.Supported));
    public string Notice => _info?.Notice ?? "";
    public bool HasNotice => !string.IsNullOrEmpty(_info?.Notice);
    /// <summary>A switch waits for its image: it is used from the next start.</summary>
    public bool IsSwitching => _info is not null && _info.TargetMajor != _info.Major;
    public string SwitchingText => _info is null ? "" : string.Format(CultureInfo.CurrentCulture,
        _text.Get("Android_Switching"), _info.TargetMajor);
    public string? Error => _error;
    public bool HasError => _error is not null;
    public bool IsBusy => _busy;

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

    /// <summary>Chooses <paramref name="choice"/>; <paramref name="eraseData"/> is the user's
    /// agreement that an older major erases the data.</summary>
    public async Task UseAsync(AndroidVersionChoice choice, bool eraseData)
    {
        await RunAsync(UseArguments(choice, eraseData));
    }

    private async Task RunAsync(string[] args)
    {
        _busy = true;
        Changed(nameof(IsBusy));
        Changed(nameof(CanApply));
        try
        {
            var result = await _cli.RunAsync(args).ConfigureAwait(true);
            if (result.ExitCode != 0)
            {
                var detail = result.Error.StartsWith("dsa: ", StringComparison.Ordinal) ? result.Error[5..] : result.Error;
                var problem = detail.Contains("cannot reach the DSA service", StringComparison.Ordinal)
                    ? Problem.ServiceUnreachable
                    : Problem.Refused;
                throw new DsaException(problem, detail);
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
