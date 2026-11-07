using System.Globalization;
using System.Text.Json;
using System.Text.RegularExpressions;

namespace Dsa.Settings.Core;

/// <summary>The "Run as ARM" switch of an app, as dsa apps list reports it: whether the app
/// runs its ARM code now, whether the switch can be changed, and if not, why
/// (<c>system-app</c>, <c>no-arm-libraries</c>, <c>translation-off</c> or <c>arm-only</c>).</summary>
public sealed record RunAsArmSwitch(bool On, bool Available, string Reason)
{
    /// <summary>The switch dsa says nothing of, as an older dsa: off, and not to be changed.</summary>
    public static RunAsArmSwitch Unknown { get; } = new(false, false, "unknown");
}

/// <summary>How a change of the "Run as ARM" switch went: <paramref name="Status"/> is dsa's
/// stable reason (<c>done</c>, <c>data-would-be-erased</c>, <c>not-installed</c>,
/// <c>system-app</c>, <c>no-libraries</c>, <c>translation-off</c> or <c>other</c>),
/// <paramref name="Message"/> Android's own words.</summary>
public sealed record RunAsArmReport(bool Done, string Status, string Message, bool On);

/// <summary>One app installed in Android, as dsa apps list reports it.</summary>
public sealed record InstalledAppInfo(
    string Package,
    string Label,
    string VersionName,
    long VersionCode,
    ulong FirstInstallMs,
    ulong LastUpdateMs,
    bool SystemApp,
    ulong AppBytes,
    ulong DataBytes,
    ulong CacheBytes,
    byte[] IconPng,
    RunAsArmSwitch? RunAsArm = null)
{
    public ulong TotalBytes => AppBytes + DataBytes + CacheBytes;
}

/// <summary>The user's window settings for one app, as dsa app-window reports them: whether
/// its windows keep their size, and the size they open at in density-independent pixels
/// (null for the size Android gives the task).</summary>
public sealed record AppWindowSetting(string Package, bool FixedSize, int? StartWidth, int? StartHeight)
{
    public static AppWindowSetting Default(string package) => new(package, false, null, null);

    /// <summary>The start size as dsa app-window --start-size takes it.</summary>
    public string StartSizeArgument => StartWidth is { } width && StartHeight is { } height
        ? string.Create(CultureInfo.InvariantCulture, $"{width}x{height}")
        : "auto";
}

/// <summary>A start size the Apps page offers, in density-independent pixels; null for the
/// size Android gives the task.</summary>
public sealed record StartSizeChoice(string Label, int? Width, int? Height);

/// <summary>How an uninstall went: <paramref name="Status"/> is dsa's stable reason
/// (<c>uninstalled</c>, <c>not-installed</c>, <c>system-app</c>, <c>blocked</c> or <c>other</c>),
/// <paramref name="Message"/> Android's own words.</summary>
public sealed record UninstallReport(bool Uninstalled, string Status, string Message);

/// <summary>Asks dsa about the apps installed in Android, and acts on them: opening one,
/// opening Android's settings of one or its list of apps, and uninstalling one.</summary>
public sealed class AppsService
{
    private readonly IDsaCli _cli;

    public AppsService(IDsaCli cli)
    {
        _cli = cli;
    }

    /// <summary>The start of dsa's line for an app Android did not uninstall: the reason's
    /// code, then the detail.</summary>
    private static readonly Regex Refusal = new(@"^not uninstalled \(([a-z-]+)\): (.*)$", RegexOptions.Singleline);

    /// <summary>The start of dsa's line for an app whose ABI did not change: the reason's
    /// code, then the detail.</summary>
    private static readonly Regex NotChanged = new(@"^not changed \(([a-z-]+)\): (.*)$", RegexOptions.Singleline);

    private static string Detail(CliResult result) =>
        result.Error.StartsWith("dsa: ", StringComparison.Ordinal) ? result.Error["dsa: ".Length..] : result.Error;

    private static DsaException Failure(CliResult result)
    {
        var detail = Detail(result);
        var problem = detail.Contains("cannot reach the DSA service", StringComparison.Ordinal)
            ? Problem.ServiceUnreachable
            : Problem.Refused;
        return new DsaException(problem, detail.Length > 0 ? detail : $"dsa ended with code {result.ExitCode}");
    }

    private async Task<JsonElement> RunJsonAsync(params string[] args)
    {
        var all = new List<string>(args) { "--json" };
        var result = await _cli.RunAsync(all).ConfigureAwait(true);
        if (result.ExitCode != 0)
        {
            throw Failure(result);
        }
        return Parse(result.Output);
    }

    private static JsonElement Parse(string output)
    {
        try
        {
            using var document = JsonDocument.Parse(output);
            return document.RootElement.Clone();
        }
        catch (JsonException e)
        {
            throw new DsaException(Problem.Unreadable, $"dsa printed something other than JSON: {e.Message}");
        }
    }

    private static string String(JsonElement json, string name) =>
        json.TryGetProperty(name, out var value) && value.ValueKind == JsonValueKind.String ? value.GetString() ?? "" : "";

    private static ulong Number(JsonElement json, string name) =>
        json.TryGetProperty(name, out var value) && value.ValueKind == JsonValueKind.Number && value.TryGetUInt64(out var n) ? n : 0;

    private static long Signed(JsonElement json, string name) =>
        json.TryGetProperty(name, out var value) && value.ValueKind == JsonValueKind.Number && value.TryGetInt64(out var n) ? n : 0;

    private static bool Bool(JsonElement json, string name) =>
        json.TryGetProperty(name, out var value) && value.ValueKind == JsonValueKind.True;

    private static byte[] Png(JsonElement json, string name)
    {
        try
        {
            return Convert.FromBase64String(String(json, name));
        }
        catch (FormatException)
        {
            return Array.Empty<byte>();
        }
    }

    private static RunAsArmSwitch ReadRunAsArm(JsonElement app) =>
        app.TryGetProperty("run_as_arm", out var value) && value.ValueKind == JsonValueKind.Object
            ? new RunAsArmSwitch(Bool(value, "on"), Bool(value, "available"), String(value, "reason"))
            : RunAsArmSwitch.Unknown;

    public static InstalledAppInfo ReadApp(JsonElement app) => new(
        String(app, "package"),
        String(app, "label"),
        String(app, "version_name"),
        Signed(app, "version_code"),
        Number(app, "first_install_time_ms"),
        Number(app, "last_update_time_ms"),
        Bool(app, "system_app"),
        Number(app, "app_bytes"),
        Number(app, "data_bytes"),
        Number(app, "cache_bytes"),
        Png(app, "icon_png"),
        ReadRunAsArm(app));

    /// <summary>Every app installed in Android, with its size read again; Android is started
    /// first when it is not running.</summary>
    public async Task<IReadOnlyList<InstalledAppInfo>> ListAsync()
    {
        var json = await RunJsonAsync("apps", "list").ConfigureAwait(true);
        var apps = new List<InstalledAppInfo>();
        if (json.TryGetProperty("apps", out var list) && list.ValueKind == JsonValueKind.Array)
        {
            foreach (var app in list.EnumerateArray())
            {
                var read = ReadApp(app);
                if (read.Package.Length > 0)
                {
                    apps.Add(read);
                }
            }
        }
        return apps;
    }

    /// <summary>Opens the app in a window of its own.</summary>
    public async Task OpenAsync(string package)
    {
        var result = await _cli.RunAsync(new[] { "launch", package }).ConfigureAwait(true);
        if (result.ExitCode != 0)
        {
            throw Failure(result);
        }
    }

    /// <summary>Opens Android's own settings of <paramref name="package"/> (its permissions,
    /// storage and notifications), or Android's list of every app when it is null.</summary>
    public async Task OpenSettingsAsync(string? package)
    {
        if (package is null)
        {
            await RunJsonAsync("apps", "settings").ConfigureAwait(true);
        }
        else
        {
            await RunJsonAsync("apps", "settings", package).ConfigureAwait(true);
        }
    }

    /// <summary>The user's window settings for every app that has any, by package.</summary>
    public async Task<IReadOnlyDictionary<string, AppWindowSetting>> WindowSettingsAsync()
    {
        var json = await RunJsonAsync("app-window").ConfigureAwait(true);
        return ReadWindowSettings(json);
    }

    private static IReadOnlyDictionary<string, AppWindowSetting> ReadWindowSettings(JsonElement json)
    {
        var settings = new Dictionary<string, AppWindowSetting>();
        if (json.TryGetProperty("apps", out var list) && list.ValueKind == JsonValueKind.Array)
        {
            foreach (var app in list.EnumerateArray())
            {
                var package = String(app, "package");
                if (package.Length == 0)
                {
                    continue;
                }
                int? width = null, height = null;
                if (app.TryGetProperty("start_size", out var size) && size.ValueKind == JsonValueKind.Array
                    && size.GetArrayLength() == 2 && size[0].TryGetInt32(out var w) && size[1].TryGetInt32(out var h))
                {
                    (width, height) = (w, h);
                }
                settings[package] = new AppWindowSetting(package, Bool(app, "fixed_size"), width, height);
            }
        }
        return settings;
    }

    /// <summary>Replaces the window settings of one app, and returns them as the service now
    /// has them.</summary>
    public async Task<AppWindowSetting> SetWindowSettingAsync(AppWindowSetting setting)
    {
        var json = await RunJsonAsync(
            "app-window", setting.Package,
            "--fixed-size", setting.FixedSize ? "on" : "off",
            "--start-size", setting.StartSizeArgument).ConfigureAwait(true);
        return ReadWindowSettings(json).TryGetValue(setting.Package, out var now)
            ? now
            : AppWindowSetting.Default(setting.Package);
    }

    /// <summary>Installs <paramref name="package"/> again with its ARM code (<paramref name="on"/>)
    /// or its x86_64 code, keeping its data unless <paramref name="eraseData"/>. A refusal is a
    /// report with its reason; anything else that kept the request from Android is a
    /// <see cref="DsaException"/>.</summary>
    public async Task<RunAsArmReport> RunAsArmAsync(string package, bool on, bool eraseData)
    {
        var args = new List<string> { "apps", "run-as-arm", package, on ? "on" : "off" };
        if (eraseData)
        {
            args.Add("--erase-data");
        }
        args.Add("--json");
        var result = await _cli.RunAsync(args).ConfigureAwait(true);
        if (result.ExitCode != 0)
        {
            var refused = NotChanged.Match(Detail(result));
            if (refused.Success)
            {
                return new RunAsArmReport(false, refused.Groups[1].Value, refused.Groups[2].Value, !on);
            }
            throw Failure(result);
        }
        var json = Parse(result.Output);
        return new RunAsArmReport(String(json, "status") == "done", String(json, "status"), String(json, "message"), Bool(json, "on"));
    }

    /// <summary>Uninstalls <paramref name="package"/>. A refusal by Android is a report with its
    /// reason; anything else that kept the request from Android is a
    /// <see cref="DsaException"/>.</summary>
    public async Task<UninstallReport> UninstallAsync(string package)
    {
        var result = await _cli.RunAsync(new[] { "apps", "uninstall", package, "--json" }).ConfigureAwait(true);
        if (result.ExitCode != 0)
        {
            var refused = Refusal.Match(Detail(result));
            if (refused.Success)
            {
                return new UninstallReport(false, refused.Groups[1].Value, refused.Groups[2].Value);
            }
            throw Failure(result);
        }
        var json = Parse(result.Output);
        return new UninstallReport(Bool(json, "uninstalled"), String(json, "status"), String(json, "message"));
    }
}

/// <summary>One app on the Apps page.</summary>
public sealed record AppRow(
    string Package,
    string Label,
    string Detail,
    string Size,
    byte[]? Icon,
    bool CanUninstall,
    bool IsSystem,
    string? UninstallTip,
    bool Resizable = true,
    int StartSizeIndex = 0,
    bool RunAsArm = false,
    bool CanRunAsArm = false,
    string? RunAsArmTip = null)
{
    public bool NoIcon => Icon is null;

    /// <summary>Whether the page says why the app cannot run as ARM.</summary>
    public bool HasRunAsArmTip => RunAsArmTip is not null;
}

/// <summary>The Apps page: every app installed in Android, searchable, each to open, to see
/// in Android's own settings, or to uninstall; and Android's own list of apps.</summary>
public sealed class AppsViewModel : ObservableObject
{
    private readonly AppsService _service;
    private readonly ITextSource _text;
    private IReadOnlyList<InstalledAppInfo>? _apps;
    private IReadOnlyDictionary<string, AppWindowSetting> _windows = new Dictionary<string, AppWindowSetting>();
    private string _search = "";
    private string? _error;
    private string? _message;
    private bool _busy;

    public AppsViewModel(AppsService service, ITextSource text)
    {
        _service = service;
        _text = text;
    }

    public bool IsBusy => _busy;
    public bool IsIdle => !_busy;
    public string Error => _error ?? "";
    public bool HasError => _error is not null;
    public string Message => _message ?? "";
    public bool HasMessage => _message is not null;
    public bool HasNoApps => _apps is { Count: 0 };
    public bool HasNoMatches => _apps is { Count: > 0 } && Rows.Count == 0;

    /// <summary>How many apps are installed, and how many the search shows.</summary>
    public string Count => _apps is null
        ? ""
        : _search.Trim().Length == 0
            ? string.Format(CultureInfo.CurrentCulture, _text.Get("Apps_Count"), _apps.Count)
            : string.Format(CultureInfo.CurrentCulture, _text.Get("Apps_CountMatching"), Rows.Count, _apps.Count);

    /// <summary>What the user typed to find an app: matched against its name and package.</summary>
    public string Search
    {
        get => _search;
        set
        {
            if (_search == value)
            {
                return;
            }
            _search = value ?? "";
            Changed();
            Changed(nameof(Rows));
            Changed(nameof(HasNoMatches));
            Changed(nameof(Count));
        }
    }

    /// <summary>A date in milliseconds since the Unix epoch, in the user's short date format.</summary>
    public static string Date(ulong ms, CultureInfo? culture = null) =>
        DateTimeOffset.FromUnixTimeMilliseconds((long)Math.Min(ms, (ulong)long.MaxValue))
            .ToLocalTime()
            .ToString("d", culture ?? CultureInfo.CurrentCulture);

    /// <summary>A size in bytes, as the Memory page shows sizes.</summary>
    public static string Size(ulong bytes, CultureInfo? culture = null) =>
        bytes < 1024 * 1024 && bytes > 0
            ? string.Format(culture ?? CultureInfo.CurrentCulture, "{0:0} KB", Math.Ceiling(bytes / 1024.0))
            : Format.Size(bytes / 1024, culture);

    /// <summary>The apps the search matches, by name.</summary>
    public IReadOnlyList<AppRow> Rows
    {
        get
        {
            var query = _search.Trim();
            return (_apps ?? Array.Empty<InstalledAppInfo>())
                .Where(a => query.Length == 0
                    || a.Label.Contains(query, StringComparison.CurrentCultureIgnoreCase)
                    || a.Package.Contains(query, StringComparison.OrdinalIgnoreCase))
                .OrderBy(a => a.Label.Length > 0 ? a.Label : a.Package, StringComparer.CurrentCultureIgnoreCase)
                .Select(Row)
                .ToList();
        }
    }

    /// <summary>The start sizes the page offers: Android's own, then phone and tablet sizes, in
    /// density-independent pixels.</summary>
    public IReadOnlyList<StartSizeChoice> StartSizeChoices => new[]
    {
        new StartSizeChoice(_text.Get("Apps_StartSize_Auto"), null, null),
        Choice("Apps_StartSize_PhonePortrait", 412, 915),
        Choice("Apps_StartSize_PhoneLandscape", 915, 412),
        Choice("Apps_StartSize_TabletPortrait", 800, 1280),
        Choice("Apps_StartSize_TabletLandscape", 1280, 800),
    };

    private StartSizeChoice Choice(string key, int width, int height) =>
        new(string.Format(CultureInfo.CurrentCulture, _text.Get(key), width, height), width, height);

    /// <summary>The window settings of <paramref name="package"/>.</summary>
    public AppWindowSetting WindowOf(string package) =>
        _windows.TryGetValue(package, out var setting) ? setting : AppWindowSetting.Default(package);

    /// <summary>Where a start size is among <see cref="StartSizeChoices"/>; -1, none of them,
    /// for a size set another way (with dsa app-window).</summary>
    private int StartSizeIndexOf(AppWindowSetting setting)
    {
        var choices = StartSizeChoices;
        for (var i = 0; i < choices.Count; i++)
        {
            if (choices[i].Width == setting.StartWidth && choices[i].Height == setting.StartHeight)
            {
                return i;
            }
        }
        return -1;
    }

    private AppRow Row(InstalledAppInfo app)
    {
        var parts = new List<string> { app.Package };
        if (app.VersionName.Length > 0)
        {
            parts.Add(string.Format(CultureInfo.CurrentCulture, _text.Get("Apps_Version"), app.VersionName));
        }
        if (app.FirstInstallMs > 0)
        {
            parts.Add(string.Format(CultureInfo.CurrentCulture, _text.Get("Apps_Installed"), Date(app.FirstInstallMs)));
        }
        if (app.SystemApp)
        {
            parts.Add(_text.Get("Apps_System"));
        }
        var arm = app.RunAsArm ?? RunAsArmSwitch.Unknown;
        return new AppRow(
            app.Package,
            app.Label.Length > 0 ? app.Label : app.Package,
            string.Join(" · ", parts),
            Size(app.TotalBytes),
            app.IconPng.Length > 0 ? app.IconPng : null,
            !app.SystemApp,
            app.SystemApp,
            app.SystemApp ? _text.Get("Apps_SystemTip") : null,
            !WindowOf(app.Package).FixedSize,
            StartSizeIndexOf(WindowOf(app.Package)),
            arm.On,
            arm.Available,
            RunAsArmTipOf(arm));
    }

    /// <summary>Why the "Run as ARM" switch cannot be changed, in the user's language; null
    /// when it can.</summary>
    public string? RunAsArmTipOf(RunAsArmSwitch arm) =>
        arm.Available ? null : _text.Get($"Apps_RunAsArm_{(arm.Reason.Length > 0 ? arm.Reason : "unknown")}");

    /// <summary>Installs the app again with its ARM code (<paramref name="on"/>) or its x86_64
    /// code. The app keeps its data; when Android cannot keep it, <paramref name="confirmErase"/>
    /// asks the user (with the app's name) whether to go on and erase it. A refusal is the
    /// page's error, in the user's language.</summary>
    public Task SetRunAsArmAsync(string package, bool on, Func<string, Task<bool>> confirmErase) => Run(async () =>
    {
        var label = LabelOf(package);
        var report = await _service.RunAsArmAsync(package, on, eraseData: false).ConfigureAwait(true);
        if (!report.Done && report.Status == "data-would-be-erased")
        {
            if (!await confirmErase(label).ConfigureAwait(true))
            {
                return string.Format(CultureInfo.CurrentCulture, _text.Get("Apps_RunAsArmKept"), label);
            }
            report = await _service.RunAsArmAsync(package, on, eraseData: true).ConfigureAwait(true);
        }
        if (!report.Done)
        {
            var reason = _text.Get($"Apps_RunAsArmRefused_{report.Status}");
            throw new RefusedByAndroid(
                string.Format(CultureInfo.CurrentCulture, _text.Get("Apps_RunAsArmNotChanged"), label, reason, report.Message));
        }
        return string.Format(CultureInfo.CurrentCulture, _text.Get(report.On ? "Apps_RunAsArmOn" : "Apps_RunAsArmOff"), label);
    }, reload: true);

    /// <summary>Reads the user's window settings for the apps. A service that keeps none
    /// leaves every app at the defaults.</summary>
    public Task LoadWindowSettingsAsync() => Run(async () =>
    {
        try
        {
            _windows = await _service.WindowSettingsAsync().ConfigureAwait(true);
        }
        catch (DsaException e) when (e.Problem == Problem.Refused)
        {
            _windows = new Dictionary<string, AppWindowSetting>();
        }
        return null;
    }, reload: false);

    private Task SetWindowAsync(AppWindowSetting wanted) => Run(async () =>
    {
        var now = await _service.SetWindowSettingAsync(wanted).ConfigureAwait(true);
        var windows = new Dictionary<string, AppWindowSetting>(_windows);
        if (now == AppWindowSetting.Default(now.Package))
        {
            windows.Remove(now.Package);
        }
        else
        {
            windows[now.Package] = now;
        }
        _windows = windows;
        return string.Format(CultureInfo.CurrentCulture, _text.Get("Apps_WindowChanged"), LabelOf(now.Package));
    }, reload: false);

    /// <summary>Lets the user resize the app's windows, or keeps them at the size they open
    /// at. Windows that open from now on follow it.</summary>
    public Task SetResizableAsync(string package, bool resizable) =>
        SetWindowAsync(WindowOf(package) with { FixedSize = !resizable });

    /// <summary>Opens the app's windows at the start size at <paramref name="index"/> of
    /// <see cref="StartSizeChoices"/>.</summary>
    public Task SetStartSizeAsync(string package, int index)
    {
        var choices = StartSizeChoices;
        if (index < 0 || index >= choices.Count)
        {
            return Task.CompletedTask;
        }
        var choice = choices[index];
        var current = WindowOf(package);
        if (current.StartWidth == choice.Width && current.StartHeight == choice.Height)
        {
            return Task.CompletedTask;
        }
        return SetWindowAsync(current with { StartWidth = choice.Width, StartHeight = choice.Height });
    }

    /// <summary>The name an app goes by on the page.</summary>
    public string LabelOf(string package) =>
        _apps?.FirstOrDefault(a => a.Package == package) is { Label.Length: > 0 } app ? app.Label : package;

    private async Task Run(Func<Task<string?>> work, bool reload)
    {
        if (_busy)
        {
            return;
        }
        _busy = true;
        _message = null;
        ChangedAll();
        try
        {
            _message = await work().ConfigureAwait(true);
            if (reload)
            {
                _apps = await _service.ListAsync().ConfigureAwait(true);
            }
            _error = null;
        }
        catch (RefusedByAndroid e)
        {
            _error = e.Message;
        }
        catch (Exception e)
        {
            _error = Format.Problem(_text, e);
        }
        finally
        {
            _busy = false;
        }
        ChangedAll();
    }

    /// <summary>Reads the installed apps.</summary>
    public Task RefreshAsync() => Run(() => Task.FromResult<string?>(null), reload: true);

    /// <summary>Opens the app in a window of its own.</summary>
    public Task OpenAsync(string package) => Run(async () =>
    {
        await _service.OpenAsync(package).ConfigureAwait(true);
        return null;
    }, reload: false);

    /// <summary>Opens Android's own page of the app: its permissions, storage and
    /// notifications.</summary>
    public Task InfoAsync(string package) => Run(async () =>
    {
        await _service.OpenSettingsAsync(package).ConfigureAwait(true);
        return null;
    }, reload: false);

    /// <summary>Opens Android's own list of every app.</summary>
    public Task ManageAsync() => Run(async () =>
    {
        await _service.OpenSettingsAsync(null).ConfigureAwait(true);
        return null;
    }, reload: false);

    /// <summary>Uninstalls the app; the page asks the user first. A refusal is the page's
    /// error, in the user's language.</summary>
    public Task UninstallAsync(string package) => Run(async () =>
    {
        var label = LabelOf(package);
        var report = await _service.UninstallAsync(package).ConfigureAwait(true);
        if (!report.Uninstalled)
        {
            var reason = _text.Get($"Apps_Refused_{report.Status}");
            throw new RefusedByAndroid(
                string.Format(CultureInfo.CurrentCulture, _text.Get("Apps_NotUninstalled"), label, reason, report.Message));
        }
        return string.Format(CultureInfo.CurrentCulture, _text.Get("Apps_Uninstalled"), label);
    }, reload: true);

    /// <summary>Android refused; the message says why in the user's language.</summary>
    private sealed class RefusedByAndroid : Exception
    {
        public RefusedByAndroid(string message) : base(message)
        {
        }
    }
}
