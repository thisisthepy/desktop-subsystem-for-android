using System.ComponentModel;
using System.Globalization;
using System.Runtime.CompilerServices;

namespace Dsa.Settings.Core;

/// <summary>Looks up a text in the user's language. The app reads its .resw resources; the
/// tests give the key back.</summary>
public interface ITextSource
{
    string Get(string key);
}

public abstract class ObservableObject : INotifyPropertyChanged
{
    public event PropertyChangedEventHandler? PropertyChanged;

    protected void Changed([CallerMemberName] string? name = null) =>
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name));

    protected void ChangedAll() => PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(string.Empty));
}

public static class Format
{
    /// <summary>A size in KiB as MB below 1 GB and as GB above, in the user's number format.</summary>
    public static string Size(ulong kb, CultureInfo? culture = null)
    {
        culture ??= CultureInfo.CurrentCulture;
        double mb = kb / 1024.0;
        return mb >= 1024
            ? string.Format(culture, "{0:0.00} GB", mb / 1024.0)
            : string.Format(culture, "{0:0.0} MB", mb);
    }

    /// <summary>What went wrong, in the user's language, with what dsa said after it.</summary>
    public static string Problem(ITextSource text, Exception error)
    {
        if (error is DsaException dsa)
        {
            var key = dsa.Problem switch
            {
                Core.Problem.CliMissing => "Problem_CliMissing",
                Core.Problem.ServiceUnreachable => "Problem_ServiceUnreachable",
                Core.Problem.Unreadable => "Problem_Unreadable",
                _ => "Problem_Refused",
            };
            return $"{text.Get(key)} ({dsa.Message})";
        }
        return $"{text.Get("Problem_Refused")} ({error.Message})";
    }
}

/// <summary>The General page: the state of Android, starting and stopping it, and the last
/// error.</summary>
public sealed class GeneralViewModel : ObservableObject
{
    private readonly DsaService _service;
    private readonly ITextSource _text;
    private StatusInfo? _status;
    private bool _busy;
    private string? _error;

    public GeneralViewModel(DsaService service, ITextSource text)
    {
        _service = service;
        _text = text;
    }

    public SessionState State => _status?.State ?? SessionState.Unknown;
    public string StateText => _text.Get($"State_{State}");
    public string LastReason => _status?.LastReason ?? "";
    public bool HasLastReason => !string.IsNullOrEmpty(_status?.LastReason);
    public string Problem => _status?.Problem ?? "";
    public bool HasProblem => !string.IsNullOrEmpty(_status?.Problem);
    public string UpdateNotice => _status?.UpdateNotice ?? "";
    public bool HasUpdateNotice => !string.IsNullOrEmpty(_status?.UpdateNotice);
    public bool IsBusy => _busy;
    public bool CanStart => !_busy && State == SessionState.Stopped;
    public bool CanStop => !_busy && State is SessionState.Starting or SessionState.Stalled or SessionState.Active
        or SessionState.Dozing or SessionState.Frozen;
    public string Error => _error ?? "";
    public bool HasError => _error is not null;

    private async Task RunAsync(Func<Task<StatusInfo>> action)
    {
        _busy = true;
        _error = null;
        ChangedAll();
        try
        {
            _status = await action().ConfigureAwait(true);
        }
        catch (Exception e)
        {
            _error = Format.Problem(_text, e);
        }
        finally
        {
            _busy = false;
            ChangedAll();
        }
    }

    public Task RefreshAsync() => RunAsync(_service.StatusAsync);

    /// <summary>Reads the state again without marking the page busy, as a timer does while
    /// the page is shown. Does nothing while a start or a stop is under way.</summary>
    public async Task PollAsync()
    {
        if (_busy)
        {
            return;
        }
        try
        {
            var status = await _service.StatusAsync().ConfigureAwait(true);
            if (_busy)
            {
                return;
            }
            _status = status;
            _error = null;
        }
        catch (Exception e)
        {
            _error = Format.Problem(_text, e);
        }
        ChangedAll();
    }

    public Task StartAsync() => RunAsync(_service.StartAsync);
    public Task StopAsync() => RunAsync(_service.StopAsync);
}

/// <summary>One choice of render GPU: what the list shows and the value the setting keeps.</summary>
public sealed record GpuChoice(string Label, string Value);

/// <summary>The settings the Display, Updates, Advanced and About pages show and change.</summary>
public sealed class SettingsViewModel : ObservableObject
{
    private readonly DsaService _service;
    private readonly ITextSource _text;
    private SettingsInfo? _settings;
    private IReadOnlyList<GpuInfo> _gpus = Array.Empty<GpuInfo>();
    private string? _error;
    private bool _busy;
    private bool _administrator;

    public SettingsViewModel(DsaService service, ITextSource text)
    {
        _service = service;
        _text = text;
    }

    public SettingsInfo? Settings => _settings;
    public bool Loaded => _settings is not null;
    public bool IsBusy => _busy;
    public string Error => _error ?? "";
    public bool HasError => _error is not null;
    public bool Freeze => _settings?.Freeze ?? false;
    /// <summary>Whether web links apps in Android open go to Android apps instead of the
    /// default browser.</summary>
    public bool LinksInAndroid => _settings?.LinksInAndroid ?? false;
    /// <summary>Whether text copied on Windows can be pasted in Android, and the reverse.</summary>
    public bool Clipboard => _settings?.Clipboard ?? true;
    /// <summary>Whether adb reaches Android through the loopback port.</summary>
    public bool DeveloperMode => _settings?.DeveloperMode ?? false;
    public int AdbPort => _settings?.AdbPort ?? SettingsInfo.DefaultAdbPort;
    /// <summary>The command that connects adb, shown while developer mode is on.</summary>
    public string AdbConnect => _settings?.AdbConnect ?? $"adb connect 127.0.0.1:{SettingsInfo.DefaultAdbPort}";
    public bool ProgramUpdates => _settings?.ProgramUpdates ?? false;
    public bool ImageUpdates => _settings?.ImageUpdates ?? false;
    public bool ProgramUpdatesAvailable => _settings?.ProgramUpdatesAvailable ?? false;
    public bool ImageUpdatesAvailable => _settings?.ImageUpdatesAvailable ?? false;
    /// <summary>Whether the app runs as an administrator (an elevated process). The update
    /// settings apply to every user, so the service lets only an administrator change
    /// them.</summary>
    public bool IsAdministrator
    {
        get => _administrator;
        set
        {
            _administrator = value;
            ChangedAll();
        }
    }

    public bool CanChangeProgramUpdates => Loaded && !_busy && ProgramUpdatesAvailable && _administrator;
    public bool CanChangeImageUpdates => Loaded && !_busy && ImageUpdatesAvailable && _administrator;
    /// <summary>Whether a switch can be changed only by running dsa as an administrator.</summary>
    public bool ProgramUpdatesNeedAdministrator => Loaded && !_busy && ProgramUpdatesAvailable && !_administrator;
    public bool ImageUpdatesNeedAdministrator => Loaded && !_busy && ImageUpdatesAvailable && !_administrator;
    public bool NeedsAdministrator => Loaded && !_administrator && (ProgramUpdatesAvailable || ImageUpdatesAvailable);
    /// <summary>Whether the newest version can be installed now; any user may ask.</summary>
    public bool CanUpdateNow => Loaded && !_busy && ProgramUpdatesAvailable;
    public bool CanChange => Loaded && !_busy;
    public string ServiceVersion => _settings?.Version ?? "";
    public string AndroidVersion => _settings?.AndroidVersion ?? "";
    public bool HasAndroidVersion => !string.IsNullOrEmpty(_settings?.AndroidVersion);
    public string LogDir => _settings?.LogDir ?? "";
    public bool HasLogDir => !string.IsNullOrEmpty(_settings?.LogDir);
    public string RenderGpu => _settings?.RenderGpu ?? "display";

    /// <summary>The GPU that drives the primary monitor, when it is known.</summary>
    public string DisplayGpuName => _gpus.FirstOrDefault(g => g.PrimaryDisplay)?.Name ?? "";

    /// <summary>The three kinds, then each GPU of this machine by name.</summary>
    public IReadOnlyList<GpuChoice> GpuChoices
    {
        get
        {
            var choices = new List<GpuChoice>
            {
                new(_text.Get("Gpu_Display"), "display"),
                new(_text.Get("Gpu_Integrated"), "integrated"),
                new(_text.Get("Gpu_Discrete"), "discrete"),
            };
            foreach (var gpu in _gpus)
            {
                if (!choices.Any(c => c.Value == gpu.Name))
                {
                    choices.Add(new GpuChoice(gpu.Name, gpu.Name));
                }
            }
            // A name chosen before that matches no GPU found now still shows as chosen.
            if (!choices.Any(c => string.Equals(c.Value, RenderGpu, StringComparison.OrdinalIgnoreCase)))
            {
                choices.Add(new GpuChoice(RenderGpu, RenderGpu));
            }
            return choices;
        }
    }

    public int SelectedGpuIndex
    {
        get
        {
            var choices = GpuChoices;
            for (var i = 0; i < choices.Count; i++)
            {
                if (string.Equals(choices[i].Value, RenderGpu, StringComparison.OrdinalIgnoreCase))
                {
                    return i;
                }
            }
            return 0;
        }
    }

    private async Task ChangeAsync(Func<Task<SettingsInfo>> change)
    {
        _busy = true;
        _error = null;
        ChangedAll();
        try
        {
            _settings = await change().ConfigureAwait(true);
        }
        catch (Exception e)
        {
            // The values stay as the service last reported them, so a switch moves back.
            _error = Format.Problem(_text, e);
        }
        finally
        {
            _busy = false;
            ChangedAll();
        }
    }

    public async Task LoadAsync()
    {
        await ChangeAsync(_service.SettingsAsync).ConfigureAwait(true);
        try
        {
            _gpus = await _service.GpusAsync().ConfigureAwait(true);
        }
        catch (DsaException)
        {
            // The list only helps to choose; the kinds still work without it.
            _gpus = Array.Empty<GpuInfo>();
        }
        ChangedAll();
    }

    public Task SetRenderGpuAsync(string value)
    {
        if (string.Equals(value, RenderGpu, StringComparison.OrdinalIgnoreCase) && Loaded)
        {
            return Task.CompletedTask;
        }
        return ChangeAsync(() => _service.SetRenderGpuAsync(value));
    }

    public Task SetFreezeAsync(bool on) =>
        on == Freeze && Loaded ? Task.CompletedTask : ChangeAsync(() => _service.SetFreezeAsync(on));

    public Task SetLinksInAndroidAsync(bool on) =>
        on == LinksInAndroid && Loaded ? Task.CompletedTask : ChangeAsync(() => _service.SetLinksInAndroidAsync(on));

    public Task SetClipboardAsync(bool on) =>
        on == Clipboard && Loaded ? Task.CompletedTask : ChangeAsync(() => _service.SetClipboardAsync(on));

    public Task SetDeveloperModeAsync(bool on) =>
        on == DeveloperMode && Loaded ? Task.CompletedTask : ChangeAsync(() => _service.SetDeveloperModeAsync(on));

    /// <summary>Picks the loopback port adb connects to. Text that is not a port from 1 to
    /// 65535 changes nothing.</summary>
    public Task SetAdbPortAsync(string text)
    {
        if (!int.TryParse(text.Trim(), System.Globalization.NumberStyles.None,
                System.Globalization.CultureInfo.InvariantCulture, out var port) || port is < 1 or > 65535)
        {
            ChangedAll();
            return Task.CompletedTask;
        }
        return port == AdbPort && Loaded ? Task.CompletedTask : ChangeAsync(() => _service.SetAdbPortAsync(port));
    }

    public Task SetProgramUpdatesAsync(bool on) =>
        on == ProgramUpdates && Loaded ? Task.CompletedTask : ChangeAsync(() => _service.SetProgramUpdatesAsync(on));

    public Task SetImageUpdatesAsync(bool on) =>
        on == ImageUpdates && Loaded ? Task.CompletedTask : ChangeAsync(() => _service.SetImageUpdatesAsync(on));
}

/// <summary>How the install bar looks.</summary>
public enum InstallSeverity
{
    Informational,
    Success,
    Error,
}

/// <summary>APK files dropped on the window: each is installed in Android in turn, and the bar
/// at the top of the window says how it went.</summary>
public sealed class InstallViewModel : ObservableObject
{
    private readonly DsaService _service;
    private readonly ITextSource _text;
    private bool _busy;
    private bool _open;
    private InstallSeverity _severity;
    private string _title = "";
    private string _message = "";

    public InstallViewModel(DsaService service, ITextSource text)
    {
        _service = service;
        _text = text;
    }

    public bool IsBusy => _busy;
    /// <summary>Whether the bar shows; its close button sets it to false.</summary>
    public bool IsOpen
    {
        get => _open;
        set
        {
            _open = value;
            Changed();
        }
    }
    public InstallSeverity Severity => _severity;
    public string Title => _title;
    public string Message => _message;

    /// <summary>Whether <paramref name="path"/> names an APK file.</summary>
    public static bool IsApk(string path) =>
        path.EndsWith(".apk", StringComparison.OrdinalIgnoreCase);

    /// <summary>Whether a drop of <paramref name="paths"/> would install anything, so the
    /// window shows a drop is welcome.</summary>
    public bool Accepts(IEnumerable<string>? paths) => !_busy && paths is not null && paths.Any(IsApk);

    private void Show(InstallSeverity severity, string title, string message)
    {
        _severity = severity;
        _title = title;
        _message = message;
        _open = true;
        ChangedAll();
    }

    /// <summary>Installs every APK of <paramref name="paths"/>, one after the other, and says
    /// how each went. Files that are not APKs are named and left alone.</summary>
    public async Task DropAsync(IReadOnlyList<string> paths)
    {
        if (_busy)
        {
            return;
        }
        var apks = paths.Where(IsApk).ToList();
        var others = paths.Where(p => !IsApk(p)).Select(p => Path.GetFileName(p)).ToList();
        if (apks.Count == 0)
        {
            Show(InstallSeverity.Error, _text.Get("Install_FailedTitle"),
                string.Format(CultureInfo.CurrentCulture, _text.Get("Install_NotApk"), string.Join(", ", others)));
            return;
        }
        _busy = true;
        var lines = new List<string>();
        var failed = false;
        try
        {
            foreach (var path in apks)
            {
                var name = Path.GetFileName(path);
                Show(InstallSeverity.Informational, _text.Get("Install_BusyTitle"),
                    string.Format(CultureInfo.CurrentCulture, _text.Get("Install_Busy"), name));
                try
                {
                    var report = await _service.InstallAsync(path).ConfigureAwait(true);
                    if (report.Installed)
                    {
                        lines.Add(string.Format(CultureInfo.CurrentCulture, _text.Get("Install_Installed"),
                            name, report.Package ?? ""));
                    }
                    else
                    {
                        failed = true;
                        lines.Add(string.Format(CultureInfo.CurrentCulture, _text.Get("Install_Refused"),
                            name, _text.Get($"Install_Reason_{report.Status}"), report.Detail));
                    }
                }
                catch (Exception e)
                {
                    failed = true;
                    lines.Add(string.Format(CultureInfo.CurrentCulture, _text.Get("Install_NotSent"),
                        name, Format.Problem(_text, e)));
                }
            }
            if (others.Count > 0)
            {
                lines.Add(string.Format(CultureInfo.CurrentCulture, _text.Get("Install_NotApk"), string.Join(", ", others)));
            }
        }
        finally
        {
            _busy = false;
        }
        Show(failed ? InstallSeverity.Error : InstallSeverity.Success,
            _text.Get(failed ? "Install_FailedTitle" : "Install_DoneTitle"),
            string.Join("\n", lines));
    }
}

/// <summary>A widget on offer as the Widgets page lists it: its name, its size and whether it
/// asks to be set up, and its preview, null when it has none.</summary>
public sealed record WidgetOfferRow(string Provider, string Label, string Detail, byte[]? Preview);

/// <summary>A placed widget as the Widgets page lists it.</summary>
public sealed record PlacedWidgetRow(int Id, string Label, string Detail);

/// <summary>The Widgets page: the widgets the apps in Android offer, which the user places on
/// the desktop, and the ones placed, which the user removes. Listing them starts Android when
/// it is not running.</summary>
public sealed class WidgetsViewModel : ObservableObject
{
    private readonly DsaService _service;
    private readonly ITextSource _text;
    private WidgetList? _list;
    private string? _error;
    private string? _message;
    private bool _busy;

    public WidgetsViewModel(DsaService service, ITextSource text)
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
    public bool Loaded => _list is not null;
    public bool HasNoOffers => _list is { Offers.Count: 0 };
    public bool HasNoPlaced => _list is { Placed.Count: 0 };

    public IReadOnlyList<WidgetOfferRow> Offers => (_list?.Offers ?? Array.Empty<WidgetOffer>())
        .Select(o => new WidgetOfferRow(
            o.Provider,
            o.Label.Length > 0 ? o.Label : o.Provider,
            string.Format(CultureInfo.CurrentCulture, _text.Get("Widgets_Size"), o.DefaultWidthDp, o.DefaultHeightDp)
                + (o.NeedsConfiguration ? " · " + _text.Get("Widgets_Setup") : ""),
            o.PreviewPng.Length > 0 ? o.PreviewPng : null))
        .ToList();

    public IReadOnlyList<PlacedWidgetRow> Placed => (_list?.Placed ?? Array.Empty<PlacedWidget>())
        .Select(w => new PlacedWidgetRow(
            w.Id,
            w.Label.Length > 0 ? w.Label : w.Provider,
            string.Format(CultureInfo.CurrentCulture, _text.Get("Widgets_At"), w.X, w.Y, w.WidthDp, w.HeightDp)))
        .ToList();

    private async Task Run(Func<Task<string?>> work)
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
            _list = await _service.WidgetsAsync().ConfigureAwait(true);
            _error = null;
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

    /// <summary>Reads the widgets on offer and the ones placed.</summary>
    public Task RefreshAsync() => Run(() => Task.FromResult<string?>(null));

    /// <summary>Places a widget of <paramref name="provider"/> on the desktop.</summary>
    public Task AddAsync(string provider) => Run(async () =>
    {
        var placed = await _service.AddWidgetAsync(provider).ConfigureAwait(true);
        return string.Format(CultureInfo.CurrentCulture, _text.Get("Widgets_Placed"),
            placed.Label.Length > 0 ? placed.Label : placed.Provider);
    });

    /// <summary>Removes the widget <paramref name="id"/> from the desktop.</summary>
    public Task RemoveAsync(int id) => Run(async () =>
    {
        var label = _list?.Placed.FirstOrDefault(w => w.Id == id)?.Label ?? id.ToString(CultureInfo.CurrentCulture);
        await _service.RemoveWidgetAsync(id).ConfigureAwait(true);
        return string.Format(CultureInfo.CurrentCulture, _text.Get("Widgets_Removed"), label);
    });
}

/// <summary>One line of the memory page: what it is, a detail, and its size.</summary>
public sealed record MemoryRow(string Label, string Detail, string Size);

/// <summary>The Memory page: the host processes that run Android, and Android's own
/// breakdown.</summary>
public sealed class MemoryViewModel : ObservableObject
{
    private readonly DsaService _service;
    private readonly ITextSource _text;
    private MemoryReport? _report;
    private string? _error;

    public MemoryViewModel(DsaService service, ITextSource text)
    {
        _service = service;
        _text = text;
    }

    public MemoryReport? Report => _report;
    public string Error => _error ?? "";
    public bool HasError => _error is not null;
    public string HostTotal => Format.Size(_report?.HostTotalKb ?? 0);
    public bool HostRunning => _report is { Host.Count: > 0 };
    /// <summary>Whether the report says no host process of the user's Android runs.</summary>
    public bool HostNotRunning => _report is not null && !HostRunning;
    public bool GuestReported => _report?.Guest is not null;
    public bool GuestNotReported => !GuestReported;
    public string GuestTotal => _report?.Guest is { } guest ? Format.Size(guest.TotalKb) : "";
    public string GuestAvailable => _report?.Guest is { } guest ? Format.Size(guest.AvailableKb) : "";

    /// <summary>How full the guest's memory is, from 0 to 100.</summary>
    public double GuestUsedPercent => _report?.Guest is { TotalKb: > 0 } guest
        ? 100.0 * (guest.TotalKb - Math.Min(guest.AvailableKb, guest.TotalKb)) / guest.TotalKb
        : 0;

    public string GuestAge => _report?.Guest is { } guest
        ? string.Format(CultureInfo.CurrentCulture, _text.Get("Memory_ReportAge"), guest.AgeSeconds)
        : "";

    public IReadOnlyList<MemoryRow> HostRows => (_report?.Host ?? Array.Empty<HostProcess>())
        .Select(p => new MemoryRow(
            _text.Get($"Role_{p.Role}"),
            string.Format(CultureInfo.CurrentCulture, _text.Get("Memory_ProcessDetail"), p.Pid, Format.Size(p.PrivateKb)),
            Format.Size(p.WorkingSetKb)))
        .ToList();

    /// <summary>Android's breakdown: the apps and the system, the caches, the kernel's
    /// reclaimable caches, shared memory and compressed swap, then the largest processes.</summary>
    public IReadOnlyList<MemoryRow> GuestRows
    {
        get
        {
            if (_report?.Guest is not { } guest)
            {
                return Array.Empty<MemoryRow>();
            }
            var rows = new List<MemoryRow>
            {
                new(_text.Get("Memory_Apps"), _text.Get("Memory_AppsDetail"), Format.Size(guest.TotalPssKb)),
                new(_text.Get("Memory_Cache"), _text.Get("Memory_CacheDetail"), Format.Size(guest.CachedKb + guest.BuffersKb)),
                new(_text.Get("Memory_Kernel"), _text.Get("Memory_KernelDetail"), Format.Size(guest.SlabReclaimableKb)),
                new(_text.Get("Memory_Shared"), "", Format.Size(guest.ShmemKb)),
                new(_text.Get("Memory_Zram"),
                    string.Format(CultureInfo.CurrentCulture, _text.Get("Memory_ZramDetail"), Format.Size(guest.ZramOriginalKb)),
                    Format.Size(guest.ZramUsedKb)),
                new(_text.Get("Memory_Free"), "", Format.Size(guest.FreeKb)),
            };
            return rows;
        }
    }

    public IReadOnlyList<MemoryRow> GuestProcessRows => (_report?.Guest?.TopProcesses ?? Array.Empty<GuestProcess>())
        .Select(p => new MemoryRow(
            p.Name,
            string.Format(CultureInfo.CurrentCulture, _text.Get("Memory_GuestProcessDetail"), p.Pid),
            Format.Size(p.PssKb)))
        .ToList();

    public async Task RefreshAsync()
    {
        try
        {
            _report = await _service.MemoryAsync().ConfigureAwait(true);
            _error = null;
        }
        catch (Exception e)
        {
            _error = Format.Problem(_text, e);
        }
        ChangedAll();
    }
}
