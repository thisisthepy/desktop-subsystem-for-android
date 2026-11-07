using System.Collections.ObjectModel;
using System.Text.Json;
using System.Text.RegularExpressions;

namespace Dsa.Settings.Core;

/// <summary>One value of the device identity on the page: what the user typed, what Android
/// reports when it is left empty, and why the typed value is not allowed, if it is not.</summary>
public sealed class IdentityField : ObservableObject
{
    private string _value = "";
    private string _saved = "";
    private string _effective = "";
    private readonly ITextSource _text;
    private readonly Action _edited;

    internal IdentityField(string key, ITextSource text, Action edited)
    {
        Key = key;
        _text = text;
        _edited = edited;
    }

    /// <summary>The name dsa takes: <c>dsa settings set device.&lt;Key&gt;</c>.</summary>
    public string Key { get; }
    /// <summary>The key in dsa's JSON.</summary>
    public string JsonKey => Key.Replace('.', '_');
    public string Label => _text.Get($"Identity_{JsonKey}");
    public string Description => _text.Get($"Identity_{JsonKey}_Description");

    /// <summary>What the user typed; empty means the default: the computer's value or DSA's.</summary>
    public string Value
    {
        get => _value;
        set
        {
            _value = value ?? "";
            Changed();
            Changed(nameof(Problem));
            Changed(nameof(HasProblem));
            Changed(nameof(IsEdited));
            _edited();
        }
    }

    /// <summary>What Android reports when the box is empty, shown in it as a hint.</summary>
    public string Placeholder => _effective;
    public bool IsEdited => _value != _saved;
    public string Problem => DeviceIdentityRules.Check(Key, _value) is { } key ? _text.Get(key) : "";
    public bool HasProblem => Problem.Length > 0;

    internal string Saved => _saved;

    /// <summary>Where the default came from: <c>computer</c> or <c>dsa</c>.</summary>
    public string Origin { get; private set; } = "dsa";
    /// <summary>Where the default came from, for people: this computer, or DSA's own value.</summary>
    public string OriginText => DeviceIdentityViewModel.OriginText(_text, Origin);

    internal void Load(string chosen, string effective, string origin)
    {
        _saved = chosen;
        _value = chosen;
        _effective = effective;
        Origin = origin;
        ChangedAll();
    }
}

/// <summary>A value Android reports that follows the computer and is not chosen: the model
/// name, the device name and the chip.</summary>
public sealed class ComputerValue : ObservableObject
{
    private readonly ITextSource _text;

    internal ComputerValue(string key, ITextSource text)
    {
        Key = key;
        _text = text;
    }

    /// <summary>The key in dsa's JSON.</summary>
    public string Key { get; }
    public string Label => _text.Get($"Identity_{Key}");
    public string Description => _text.Get($"Identity_{Key}_Description");
    /// <summary>The value; when the computer does not say, a note that Android keeps its own.</summary>
    public string Value { get; private set; } = "";
    public string Origin { get; private set; } = "dsa";
    public string OriginText => DeviceIdentityViewModel.OriginText(_text, Origin);

    internal void Load(string value, string origin)
    {
        Value = value.Length > 0 ? value : _text.Get("Identity_NotSaid");
        Origin = origin;
        ChangedAll();
    }
}

/// <summary>The rules the service applies to each value, so the page can say what is wrong
/// before anything is sent. The service checks again; what it refuses is shown as an
/// error.</summary>
public static class DeviceIdentityRules
{
    private static readonly Regex Printable = new(@"^[\x20-\x7E]{1,64}$");
    private static readonly Regex NameLike = new(@"^[A-Za-z0-9][A-Za-z0-9._-]{0,31}$");
    private static readonly Regex Part = new(@"^[A-Za-z0-9._-]{1,64}$");
    private static readonly Regex Tags = new(@"^[A-Za-z0-9._,-]{1,64}$");
    private static readonly Regex Characteristics = new(@"^[a-z0-9_]+(,[a-z0-9_]+)*$");

    /// <summary>The text key of what is wrong with <paramref name="value"/> for the field
    /// <paramref name="key"/>, or null when it is allowed. Empty is always allowed: it is the
    /// default.</summary>
    public static string? Check(string key, string value)
    {
        if (value.Length == 0)
        {
            return null;
        }
        var ok = key switch
        {
            "manufacturer" or "model" => Printable.IsMatch(value) && value.Trim() == value,
            "brand" or "device" or "name" => NameLike.IsMatch(value),
            "characteristics" => value.Length <= 64 && Characteristics.IsMatch(value),
            "fingerprint" => IsFingerprint(value),
            "fingerprint.release" or "fingerprint.id" or "fingerprint.incremental" => Part.IsMatch(value),
            "fingerprint.type" => value is "user" or "userdebug" or "eng",
            "fingerprint.tags" => Tags.IsMatch(value),
            _ => false,
        };
        return ok ? null : $"Identity_{key.Replace('.', '_')}_Invalid";
    }

    /// <summary>Whether <paramref name="value"/> reads
    /// brand/name/device:release/id/incremental:type/tags with allowed parts.</summary>
    public static bool IsFingerprint(string value)
    {
        if (value.Length > 256)
        {
            return false;
        }
        var sections = value.Split(':');
        if (sections.Length != 3)
        {
            return false;
        }
        var product = sections[0].Split('/');
        var build = sections[1].Split('/');
        var kind = sections[2].Split('/');
        return product.Length == 3 && build.Length == 3 && kind.Length == 2
            && product.All(NameLike.IsMatch)
            && build.All(Part.IsMatch)
            && kind[0] is "user" or "userdebug" or "eng"
            && Tags.IsMatch(kind[1]);
    }
}

/// <summary>The Device identity page: the names and build fingerprint Android reports to its
/// apps, which the user may change. Android reads them when it starts, so a saved change
/// applies from its next start; the page offers to restart Android then.</summary>
public sealed class DeviceIdentityViewModel : ObservableObject
{
    /// <summary>The fields, in the order the page shows them.</summary>
    public static readonly string[] Keys =
    {
        "manufacturer", "brand", "model", "device", "name", "characteristics",
        "fingerprint.release", "fingerprint.id", "fingerprint.incremental", "fingerprint.type",
        "fingerprint.tags", "fingerprint",
    };

    private readonly IDsaCli _cli;
    private readonly DsaService _service;
    private readonly ITextSource _text;
    private readonly Dictionary<string, IdentityField> _fields = new();
    private string? _error;
    private string? _message;
    private bool _busy;
    private bool _loaded;
    private bool _savedSinceStart;
    private SessionState _state = SessionState.Unknown;

    public DeviceIdentityViewModel(IDsaCli cli, DsaService service, ITextSource text)
    {
        _cli = cli;
        _service = service;
        _text = text;
        foreach (var key in Keys)
        {
            _fields[key] = new IdentityField(key, text, OnEdited);
        }
        Names = new ObservableCollection<IdentityField>(Keys.Take(6).Select(k => _fields[k]));
        FingerprintParts = new ObservableCollection<IdentityField>(Keys.Skip(6).Take(5).Select(k => _fields[k]));
        ComputerValues = new ObservableCollection<ComputerValue>(ComputerKeys.Select(k => new ComputerValue(k, text)));
    }

    /// <summary>The values that follow the computer, in the order the page shows them.</summary>
    public static readonly string[] ComputerKeys = { "model_name", "device_name", "soc_manufacturer", "soc_model" };

    /// <summary>The model name, the device name and the chip, which follow the computer.</summary>
    public ObservableCollection<ComputerValue> ComputerValues { get; }

    public ComputerValue Computer(string key) => ComputerValues.First(v => v.Key == key);

    internal static string OriginText(ITextSource text, string origin) =>
        text.Get(origin == "computer" ? "Identity_FromComputer" : "Identity_FromDsa");

    /// <summary>Manufacturer, brand, model, device, product name and form factor.</summary>
    public ObservableCollection<IdentityField> Names { get; }
    /// <summary>The fingerprint's release, build ID, incremental version, type and tags.</summary>
    public ObservableCollection<IdentityField> FingerprintParts { get; }
    /// <summary>The whole fingerprint string, which wins over its parts when set.</summary>
    public IdentityField WholeFingerprint => _fields["fingerprint"];

    public IdentityField Field(string key) => _fields[key];

    public string Error => _error ?? "";
    public bool HasError => _error is not null;
    public string Message => _message ?? "";
    public bool HasMessage => _message is not null;
    public bool IsBusy => _busy;
    public bool HasEdits => _fields.Values.Any(f => f.IsEdited);
    public bool HasProblems => _fields.Values.Any(f => f.HasProblem);
    public bool CanSave => _loaded && !_busy && HasEdits && !HasProblems;
    public bool CanReset => _loaded && !_busy;
    public bool IsRunning => _state is SessionState.Starting or SessionState.Active or SessionState.Dozing
        or SessionState.Frozen or SessionState.Stalled;
    /// <summary>Whether a saved change waits for Android to start again.</summary>
    public bool NeedsRestart => _savedSinceStart && IsRunning;
    public bool CanRestart => NeedsRestart && !_busy;

    /// <summary>The fingerprint Android will report from its next start, with what is typed
    /// now.</summary>
    public string FingerprintPreview
    {
        get
        {
            string Now(string key) => _fields[key].Value.Length > 0 ? _fields[key].Value : _fields[key].Placeholder;
            if (_fields["fingerprint"].Value.Length > 0)
            {
                return _fields["fingerprint"].Value;
            }
            var parts = new[] { "fingerprint.release", "fingerprint.id", "fingerprint.incremental" }.Select(Now).ToArray();
            if (parts.Any(p => p.Length == 0))
            {
                return _fields["fingerprint"].Placeholder;
            }
            return $"{Now("brand")}/{Now("name")}/{Now("device")}:{parts[0]}/{parts[1]}/{parts[2]}:{Now("fingerprint.type")}/{Now("fingerprint.tags")}";
        }
    }

    private void OnEdited()
    {
        Changed(nameof(HasEdits));
        Changed(nameof(HasProblems));
        Changed(nameof(CanSave));
        Changed(nameof(FingerprintPreview));
    }

    private async Task<JsonElement> RunJsonAsync(params string[] args)
    {
        var all = new List<string>(args) { "--json" };
        var result = await _cli.RunAsync(all).ConfigureAwait(true);
        if (result.ExitCode != 0)
        {
            var detail = result.Error.StartsWith("dsa: ", StringComparison.Ordinal) ? result.Error["dsa: ".Length..] : result.Error;
            var problem = detail.Contains("cannot reach the DSA service", StringComparison.Ordinal)
                ? Problem.ServiceUnreachable
                : Problem.Refused;
            throw new DsaException(problem, detail.Length > 0 ? detail.Trim() : $"dsa ended with code {result.ExitCode}");
        }
        try
        {
            using var document = JsonDocument.Parse(result.Output);
            return document.RootElement.Clone();
        }
        catch (JsonException e)
        {
            throw new DsaException(Problem.Unreadable, $"dsa printed something other than JSON: {e.Message}");
        }
    }

    private void Show(JsonElement report)
    {
        static string Text(JsonElement parent, string object_, string key) =>
            parent.TryGetProperty(object_, out var o) && o.ValueKind == JsonValueKind.Object
                && o.TryGetProperty(key, out var v) && v.ValueKind == JsonValueKind.String
                ? v.GetString() ?? ""
                : "";
        // A service from before the computer's identity says no source: every default is DSA's.
        string Origin(string key) => Text(report, "sources", key) is "computer" ? "computer" : "dsa";
        foreach (var field in _fields.Values)
        {
            field.Load(Text(report, "chosen", field.JsonKey), Text(report, "effective", field.JsonKey), Origin(field.JsonKey));
        }
        foreach (var value in ComputerValues)
        {
            value.Load(Text(report, "effective", value.Key), Origin(value.Key));
        }
        _loaded = true;
    }

    private async Task Run(Func<Task> work)
    {
        _busy = true;
        _error = null;
        ChangedAll();
        try
        {
            await work().ConfigureAwait(true);
        }
        catch (DsaException e)
        {
            _error = Format.Problem(_text, e);
        }
        finally
        {
            _busy = false;
            ChangedAll();
        }
    }

    private async Task ReadStateAsync()
    {
        try
        {
            _state = (await _service.StatusAsync().ConfigureAwait(true)).State;
        }
        catch (DsaException)
        {
            _state = SessionState.Unknown;
        }
    }

    public Task LoadAsync() => Run(async () =>
    {
        _message = null;
        Show(await RunJsonAsync("settings", "device").ConfigureAwait(true));
        await ReadStateAsync().ConfigureAwait(true);
    });

    /// <summary>Sends each value the user changed: a typed one is set, an emptied one goes
    /// back to the default.</summary>
    public Task SaveAsync() => Run(async () =>
    {
        if (HasProblems)
        {
            return;
        }
        JsonElement? last = null;
        foreach (var field in Keys.Select(k => _fields[k]).Where(f => f.IsEdited).ToList())
        {
            last = field.Value.Length == 0
                ? await RunJsonAsync("settings", "reset", $"device.{field.Key}").ConfigureAwait(true)
                : await RunJsonAsync("settings", "set", $"device.{field.Key}", field.Value).ConfigureAwait(true);
        }
        if (last is { } report)
        {
            Show(report);
            await ReadStateAsync().ConfigureAwait(true);
            _savedSinceStart = true;
            _message = _text.Get(IsRunning ? "Identity_SavedRestart" : "Identity_Saved");
        }
    });

    /// <summary>Puts every value back to the default: the computer's, or DSA's own where the computer says nothing.</summary>
    public Task ResetAsync() => Run(async () =>
    {
        Show(await RunJsonAsync("settings", "reset", "device").ConfigureAwait(true));
        await ReadStateAsync().ConfigureAwait(true);
        _savedSinceStart = true;
        _message = _text.Get(IsRunning ? "Identity_ResetRestart" : "Identity_Reset");
    });

    /// <summary>Stops Android and starts it again, so it reads the saved identity.</summary>
    public Task RestartAsync() => Run(async () =>
    {
        await _service.StopAsync().ConfigureAwait(true);
        _state = (await _service.StartAsync().ConfigureAwait(true)).State;
        _savedSinceStart = false;
        _message = _text.Get("Identity_Restarted");
    });
}
